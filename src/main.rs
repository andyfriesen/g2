use std::fmt;
use std::{error::Error, f32::consts::PI};

use clap::Parser;
use cpal::{StreamConfig, SampleRate};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Devices, Host, InputCallbackInfo, InputDevices, OutputCallbackInfo, StreamError,
};
use ringbuf::{Consumer, Producer, RingBuffer};

fn list_output_devices(host: &Host) -> Result<(), Box<dyn Error>> {
    let mut i = 0;
    for device in host.output_devices()? {
        println!("Output device {}: {}", i, device.name()?);
        i += 1;
    }

    Ok(())
}

fn list_input_devices(host: &Host) -> Result<(), Box<dyn Error>> {
    let mut i = 0;
    for device in host.input_devices()? {
        println!("Input device {}: {}", i, device.name()?);
        i += 1;
    }

    Ok(())
}

#[derive(Debug)]
struct NoDefaultDevice;

impl fmt::Display for NoDefaultDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NoDefaultDevice")
    }
}

impl std::error::Error for NoDefaultDevice {}

fn nth_device(
    devices: InputDevices<Devices>,
    default_device: Device,
    index: Option<usize>,
) -> Result<Device, Box<dyn Error>> {
    if let Some(ii) = index {
        let mut count = ii;
        let devices = devices;
        for d in devices {
            if 0 == count {
                return Ok(d);
            } else {
                count -= 1;
            }
        }
    }

    Ok(default_device)
}

fn nth_input_device(host: &Host, index: Option<usize>) -> Result<Device, Box<dyn Error>> {
    let default_device = host.default_input_device().ok_or(NoDefaultDevice {})?;
    nth_device(host.input_devices()?, default_device, index)
}

fn nth_output_device(host: &Host, index: Option<usize>) -> Result<Device, Box<dyn Error>> {
    let default_device = host.default_output_device().ok_or(NoDefaultDevice {})?;
    nth_device(host.output_devices()?, default_device, index)
}

fn on_error(err: StreamError) {
    let _ = err;
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, value_parser)]
    input_device: Option<usize>,

    #[clap(short, long, value_parser)]
    output_device: Option<usize>,
}

#[allow(dead_code)]
struct DelayFilter {
    decay: f32,
    producer: Producer<f32>,
    consumer: Consumer<f32>,
}

#[allow(dead_code)]
impl DelayFilter {
    fn new(delay_frames: usize, decay: f32) -> DelayFilter {
        let buffer = RingBuffer::new(delay_frames);
        let (mut producer, consumer) = buffer.split();

        while !producer.is_full() {
            producer.push(0.0).expect("Can't fill buffer?");
        }

        DelayFilter {
            decay,
            producer,
            consumer,
        }
    }

    fn filter(&mut self, sample: f32) -> f32 {
        let last = self.consumer.pop().expect("Delay buffer empty?");
        let result = sample + last * self.decay;
        self.producer
            .push(result)
            .expect("Unable to refill delay buffer?");

        return result;
    }
}

#[allow(dead_code)]
struct FlangeFilter {
    sample_rate: SampleRate,
    decay: f32,
    frequency: f32,
    amplitude: f32,
    t: f32,
    buffer: Vec<f32>,
    read_offset: usize,
}

const TWO_PI: f32 = PI * 0.5;

// TODO: Normalize t and frequency to seconds.  Requires knowing the sampling rate.
#[allow(dead_code)]
impl FlangeFilter {
    fn new(buffer_size: usize, sample_rate: SampleRate, frequency: f32, amplitude: f32, decay: f32) -> FlangeFilter {
        let mut buffer = Vec::with_capacity(buffer_size);
        for _ in 0..buffer_size {
            buffer.push(0.0);
        }

        FlangeFilter {
            sample_rate,
            decay,
            frequency,
            amplitude,
            t: 0.0,
            buffer,
            read_offset: 0,
        }
    }

    fn read_buffer(&mut self, reverse_offset: usize) -> f32 {
        let offset = if self.read_offset >= reverse_offset {
            self.read_offset - reverse_offset
        } else {
            self.buffer.len() - reverse_offset + self.read_offset
        };

        self.buffer[offset]
    }

    fn write_buffer(&mut self, sample: f32) {
        self.buffer[self.read_offset] = sample;
        self.read_offset += 1;
        if self.read_offset >= self.buffer.len() {
            self.read_offset = 0;
        }
    }

    fn offset(&self, t: f32) -> usize {
        let f = (self.frequency * t) / TWO_PI;
        let res = (f.cos() + 1.0) * self.amplitude + 1.0;

        res as usize
    }

    fn filter(&mut self, sample: f32) -> f32 {
        let reverse_offset = self.offset(self.t);

        let last = self.read_buffer(reverse_offset);

        let result = sample + last * self.decay;

        self.write_buffer(result);

        self.t += 1.0;

        result
    }
}

#[test]
fn flange_offset() {
    let frequency = TWO_PI;
    let ff = FlangeFilter::new(10000, SampleRate(48000), frequency, 100.0, 0.8);

    assert_eq!(201, ff.offset(0.0));
    assert_eq!(100, ff.offset(45.0 * TWO_PI));
    assert_eq!(1, ff.offset(90.0 * TWO_PI));
    assert_eq!(101, ff.offset(135.0 * TWO_PI));
    assert_eq!(201, ff.offset(180.0 * TWO_PI));
    assert_eq!(100, ff.offset(225.0 * TWO_PI));
    assert_eq!(1, ff.offset(270.0 * TWO_PI));
    assert_eq!(100, ff.offset(315.0 * TWO_PI));
    assert_eq!(201, ff.offset(360.0 * TWO_PI));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let host = cpal::default_host();

    list_input_devices(&host)?;
    list_output_devices(&host)?;

    let input_device = nth_input_device(&host, args.input_device)?;
    let output_device = nth_output_device(&host, args.output_device)?;

    println!("Using {}", input_device.name()?);
    println!("And {}", output_device.name()?);

    const BUFFER_SIZE: usize = 960;
    let buffer: RingBuffer<f32> = RingBuffer::new(BUFFER_SIZE);
    let (mut producer, mut consumer) = buffer.split();

    let mut supported_configs = input_device.supported_input_configs()?;

    let supported_config = supported_configs.next().unwrap().with_max_sample_rate();

    let config: StreamConfig = supported_config.into();

    println!("Sample rate {:?}", config.sample_rate);
    println!("");

    // let mut delay = DelayFilter::new(10000, 0.9);
    let mut flange = FlangeFilter::new(10000, config.sample_rate, 0.00001 * 2.0 * PI, 100.0, 0.8);

    let input_data_fn = move |data: &[f32], _cbinfo: &InputCallbackInfo| {
        for datum in data {
            producer
                .push(*datum)
                .expect("Unable to refill output buffer");
        }
    };

    let output_data_fn = move |data: &mut [f32], _cbinfo: &OutputCallbackInfo| {
        for sample in data {
            *sample = match consumer.pop() {
                // Some(s) => delay.filter(s),
                Some(s) => flange.filter(s),
                None => 0.0,
            }
        }
    };

    let input_stream = input_device.build_input_stream(&config, input_data_fn, &on_error)?;
    let output_stream = output_device.build_output_stream(&config, output_data_fn, &on_error)?;
    input_stream.play()?;
    output_stream.play()?;

    let s = &mut String::new();
    let _ = std::io::stdin().read_line(s);

    println!("Goodbye World!");

    Ok(())
}
