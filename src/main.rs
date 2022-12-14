use std::fmt;
use std::{error::Error, f32::consts::PI};

use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Devices, Host, InputCallbackInfo, InputDevices, OutputCallbackInfo, StreamError,
};
use cpal::{SampleRate, StreamConfig};
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
    /// Input device
    #[clap(short, long, value_parser)]
    input_device: Option<usize>,

    /// Output device
    #[clap(short, long, value_parser)]
    output_device: Option<usize>,

    /// List available input and output devices
    #[clap(long)]
    list: bool,
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
    amplitude: f32,

    // convert from time in samples to an input to cosine such that we hit
    // 2pi as t hits sample_rate * frequency
    offset_coefficient: f32,

    // elapsed time in samples
    t: f32,

    buffer: Vec<f32>,
    read_offset: usize,
}

#[allow(dead_code)]
impl FlangeFilter {
    fn new(
        buffer_size: usize,
        sample_rate: SampleRate,
        frequency: f32,
        amplitude: f32,
        decay: f32,
    ) -> FlangeFilter {
        let mut buffer = Vec::with_capacity(buffer_size);
        for _ in 0..buffer_size {
            buffer.push(0.0);
        }

        let SampleRate(sr) = sample_rate;

        let offset_coefficient = PI / (2.0 * frequency * sr as f32);

        FlangeFilter {
            decay,
            amplitude,
            offset_coefficient,
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
        let f = t * self.offset_coefficient;
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

// Distortion is easy: You magnify the signal, then clamp samples to make the wave more square.
#[allow(dead_code)]
struct DistortFilter {
    gain: f32,

    // Min/max value to clamp outgoing samples to.  Should be 1.0 or less.
    saturation: f32,
}

#[allow(dead_code)]
impl DistortFilter {
    fn new(gain: f32, saturation: f32) -> DistortFilter {
        DistortFilter { gain, saturation }
    }

    fn filter(&self, sample: f32) -> f32 {
        (sample * self.gain).clamp(-self.saturation, self.saturation)
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let host = cpal::default_host();

    if args.list {
        list_input_devices(&host)?;
        println!("");
        list_output_devices(&host)?;
        return Ok(());
    }

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
    // let mut filter = FlangeFilter::new(10000, config.sample_rate, 0.5, 100.0, 0.8);
    let filter = DistortFilter::new(12.0, 0.7);

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
                Some(s) => filter.filter(s),
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
