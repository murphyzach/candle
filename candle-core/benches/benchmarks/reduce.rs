use crate::benchmarks::{BenchDevice, BenchDeviceHandler};
use candle_core::{DType, Device, Storage, Tensor};
use criterion::{black_box, criterion_group, Criterion, Throughput};
use half::{bf16, f16};
use std::ops::Deref;
use std::time::Instant;

fn run_sum(a: &Tensor) {
    a.sum(2).unwrap();
}
fn run_arg_min(a: &Tensor) {
    a.argmin(2).unwrap();
}

// TODO: Remove before merging. Softmax impls live in candle-nn, so this is a temporary workaround.
fn softmax(a: &Tensor) -> candle_core::Result<()> {
    use candle_core::{backend::BackendStorage, DType};
    let (storage, layout) = a.storage_and_layout();

    let device = a.device();

    if let (Device::Metal(device), Storage::Metal(storage)) = (device, storage.deref()) {
        let command_buffer = device.command_buffer()?;
        let kernels = device.kernels();
        let name = match a.dtype() {
            DType::F32 => "softmax_f32",
            DType::F16 => "softmax_f16",
            DType::BF16 => "softmax_bf16",
            dtype => candle_core::bail!("softmax-last-dim is not implemented for {dtype:?}"),
        };

        let n = layout.stride().len();
        if !(layout.is_contiguous() && layout.stride()[n - 1] == 1) {
            candle_core::bail!("Non contiguous softmax-last-dim is not implemented");
        }

        let last_dim = layout.dims()[layout.shape().rank() - 1];
        let elem_count = layout.shape().elem_count();
        let output = device.new_buffer(elem_count, storage.dtype(), "softmax")?;
        candle_metal_kernels::call_last_softmax(
            device.metal_device(),
            &command_buffer,
            kernels,
            name,
            elem_count,
            last_dim,
            storage.buffer(),
            layout.start_offset() * storage.dtype().size_in_bytes(),
            &output,
        )
        .unwrap();
    }
    Ok(())
}

fn criterion_benchmark(c: &mut Criterion) {
    let handler = BenchDeviceHandler::new().unwrap();
    let (lo, up) = (-1000.0f32, 1000.0f32);
    for device in handler.devices {
        run_softmax(c, &device, (lo, up));
        //run_softmax(c, &device, (f16::from_f32(lo), f16::from_f32(up)));
        //run_softmax(c, &device, (bf16::from_f32(lo), bf16::from_f32(up)));

        run_reduce(c, &device, (lo, up), false);
        run_reduce(c, &device, (f16::from_f32(lo), f16::from_f32(up)), false);
        run_reduce(c, &device, (bf16::from_f32(lo), bf16::from_f32(up)), false);

        run_arg_reduce(c, &device, (lo, up), false);
        run_arg_reduce(c, &device, (f16::from_f32(lo), f16::from_f32(up)), false);
        run_arg_reduce(c, &device, (bf16::from_f32(lo), bf16::from_f32(up)), false);

        run_reduce(c, &device, (lo, up), true);
        run_reduce(c, &device, (f16::from_f32(lo), f16::from_f32(up)), true);
        run_reduce(c, &device, (bf16::from_f32(lo), bf16::from_f32(up)), true);

        run_arg_reduce(c, &device, (lo, up), true);
        run_arg_reduce(c, &device, (f16::from_f32(lo), f16::from_f32(up)), true);
        run_arg_reduce(c, &device, (bf16::from_f32(lo), bf16::from_f32(up)), true);
    }
}

fn run_softmax<T: candle_core::FloatDType>(c: &mut Criterion, device: &Device, (lo, up): (T, T)) {
    if !device.is_metal() {
        return;
    }

    let b = 1;
    let m = 1024;
    let k = 1024;
    let a = Tensor::rand(lo, up, (b, m, k), &device).unwrap();

    let flops = b * m * k * T::DTYPE.size_in_bytes();

    let name = match T::DTYPE {
        DType::F32 => "softmax_f32",
        DType::F16 => "softmax_f16",
        DType::BF16 => "softmax_bf16",
        _ => "softmax",
    };
    softmax(&a).unwrap();

    let mut group = c.benchmark_group(device.bench_name(name));
    group.throughput(Throughput::Bytes(flops as u64));
    group.bench_function("iter", move |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _i in 0..iters {
                softmax(black_box(&a)).unwrap();
            }
            device.sync().unwrap();
            start.elapsed()
        })
    });
    group.finish();
}

fn run_reduce<T: candle_core::FloatDType>(
    c: &mut Criterion,
    device: &Device,
    (lo, up): (T, T),
    strided: bool,
) {
    let b = 1;
    let m = 1024;
    let k = 1024;

    let a = if strided {
        Tensor::rand(lo, up, (b, m, k), &device)
            .unwrap()
            .transpose(0, 2)
            .unwrap()
    } else {
        Tensor::rand(lo, up, (b, m, k), &device).unwrap()
    };

    let flops = b * m * k * T::DTYPE.size_in_bytes();

    let name = match T::DTYPE {
        DType::F32 => {
            if strided {
                "reduce_f32_strided"
            } else {
                "reduce_f32"
            }
        }
        DType::F16 => {
            if strided {
                "reduce_f16_strided"
            } else {
                "reduce_f16"
            }
        }
        DType::BF16 => {
            if strided {
                "reduce_bf16_strided"
            } else {
                "reduce_bf16"
            }
        }
        _ => "reduce",
    };

    let mut group = c.benchmark_group(device.bench_name(name));
    group.throughput(Throughput::Bytes(flops as u64));
    group.bench_function("iter", move |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _i in 0..iters {
                run_sum(black_box(&a));
            }
            device.sync().unwrap();
            start.elapsed()
        })
    });
    group.finish();
}

fn run_arg_reduce<T: candle_core::FloatDType>(
    c: &mut Criterion,
    device: &Device,
    (lo, up): (T, T),
    strided: bool,
) {
    let b = 1;
    let m = 1024;
    let k = 1024;

    let a = if strided {
        Tensor::rand(lo, up, (b, m, k), &device)
            .unwrap()
            .transpose(0, 2)
            .unwrap()
    } else {
        Tensor::rand(lo, up, (b, m, k), &device).unwrap()
    };

    let flops = b * m * k * T::DTYPE.size_in_bytes();

    let name = match T::DTYPE {
        DType::F32 => {
            if strided {
                "arg_reduce_f32_strided"
            } else {
                "arg_reduce_f32"
            }
        }
        DType::F16 => {
            if strided {
                "arg_reduce_f16_strided"
            } else {
                "arg_reduce_f16"
            }
        }
        DType::BF16 => {
            if strided {
                "arg_reduce_bf16_strided"
            } else {
                "arg_reduce_bf16"
            }
        }
        _ => "unknown",
    };

    let mut group = c.benchmark_group(device.bench_name(name));
    group.throughput(Throughput::Bytes(flops as u64));
    group.bench_function("iter", move |b| {
        b.iter_custom(|iters| {
            let start = Instant::now();
            for _i in 0..iters {
                run_arg_min(black_box(&a));
            }
            device.sync().unwrap();
            start.elapsed()
        })
    });
    group.finish();
}

criterion_group!(benches, criterion_benchmark);
