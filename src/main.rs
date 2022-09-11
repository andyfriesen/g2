use std::error::Error;
use std::fmt;

use clap::Parser;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    Device, Devices, Host, InputCallbackInfo, InputDevices, StreamError, OutputCallbackInfo,
};
use ringbuf::RingBuffer;

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

// fn open_output_stream(device: &Device) -> Result<Stream, Box<dyn Error>> {
//     // Next thing: Copy the input data stream to the output
// }

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(short, long, value_parser)]
    input_device: Option<usize>,

    #[clap(short, long, value_parser)]
    output_device: Option<usize>,
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

    let input_data_fn = move |data: &[f32], _cbinfo: &InputCallbackInfo| {
        for datum in data {
            producer.push(*datum);
        }
    };

    let output_data_fn = move |data: &mut [f32], _cbinfo: &OutputCallbackInfo| {
        for sample in data {
            *sample = match consumer.pop() {
                Some(s) => s,
                None => 0.0
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
