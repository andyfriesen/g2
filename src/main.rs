use std::error::Error;
use std::fmt;

use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Devices, Host, InputCallbackInfo, InputDevices, OutputCallbackInfo, StreamError,
};
use ringbuf::{Consumer, Producer, RingBuffer};

fn list_input_devices(host: &Host) -> Result<(), Box<dyn Error>> {
    let mut i = 0;
    for device in host.input_devices()? {
        println!("Device {}: {}", i, device.name()?);
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
    decay: f32,
    frequency: f32,
    amplitude: f32,
    delay: usize,
    t: usize,
    buffer: Vec<f32>,
    read_offset: usize,
}

#[allow(dead_code)]
impl FlangeFilter {
    fn new(
        buffer_size: usize,
        frequency: f32,
        amplitude: f32,
        delay: usize,
        decay: f32,
    ) -> FlangeFilter {
        let mut buffer = Vec::with_capacity(buffer_size);
        for _ in 0..buffer_size {
            buffer.push(0.0);
        }

        FlangeFilter {
            decay,
            frequency,
            amplitude,
            delay,
            t: 0,
            buffer,
            read_offset: 0,
        }
    }

    fn offset(&self, t: usize) -> usize {
        let f = self.frequency * t as f32;
        let res: f32 = (f.cos() + 1.0) * self.amplitude;
        res as usize
    }

    fn read_buffer(&self, reverse_offset: usize) -> f32 {
        if reverse_offset <= self.read_offset {
            self.buffer[self.read_offset - reverse_offset]
        } else {
            self.buffer[self.buffer.len() - reverse_offset - self.read_offset]
        }
    }

    fn write_buffer(&mut self, sample: f32) {
        self.buffer[self.read_offset] = sample;
        self.read_offset += 1;
        if self.read_offset >= self.buffer.len() {
            self.read_offset = 0;
        }
    }

    fn filter(&mut self, sample: f32) -> f32 {
        let mut reverse_offset = self.delay;
        reverse_offset += self.offset(self.t);

        let last = self.read_buffer(reverse_offset);

        let result = sample + last * self.decay;

        self.write_buffer(result);

        self.t += 1;

        result
    }
}

#[test]
fn flange_offset() {
    let ff = FlangeFilter::new(10000, PI / 20.0, 50.0, 1000, 0.9);

    for i in 0..50 {
        println!("{}!", ff.offset(i));
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let host = cpal::default_host();

    list_input_devices(&host)?;

    let input_device = nth_input_device(&host, args.input_device)?;
    let output_device = nth_output_device(&host, args.output_device)?;

    println!("Using {}", input_device.name()?);
    println!("");

    const BUFFER_SIZE: usize = 960;
    let buffer: RingBuffer<f32> = RingBuffer::new(BUFFER_SIZE);
    let (mut producer, mut consumer) = buffer.split();

    let mut supported_configs = input_device.supported_input_configs()?;

    let supported_config = supported_configs.next().unwrap().with_max_sample_rate();

    let config = supported_config.into();

    // let mut delay = DelayFilter::new(10000, 0.9);
    let mut flange = FlangeFilter::new(10000, 0.001, 50.0, 1000, 0.7);

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
