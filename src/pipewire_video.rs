//! Experimental CPU-readback video output. The GStreamer child provides the
//! PipeWire node; neither the render loop nor the GPU waits for a consumer.
use anyhow::{Context, Result, bail};
use std::{
    io::Write,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread::{self, JoinHandle},
};

pub const WIDTH: u32 = 3840;
pub const HEIGHT: u32 = 2160;
pub const FPS: u32 = 30;
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const ROW_BYTES: u32 = WIDTH * 4;
const PADDED_ROW_BYTES: u32 =
    ROW_BYTES.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
const FRAME_BYTES: usize = (ROW_BYTES * HEIGHT) as usize;

pub struct Video {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub blitter: Option<wgpu::util::TextureBlitter>,
    buffer: wgpu::Buffer,
    pending: Option<Receiver<Result<(), wgpu::BufferAsyncError>>>,
    producer: Producer,
    failed: bool,
}

impl Video {
    pub fn new(device: &wgpu::Device, surface_format: Option<wgpu::TextureFormat>) -> Result<Self> {
        let producer = Producer::start()?;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("PipeWire spectator 4K"),
            size: extent(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PipeWire spectator readback"),
            size: u64::from(PADDED_ROW_BYTES * HEIGHT),
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        log::info!("PipeWire prototype: tomori-spectator, RGBA {WIDTH}x{HEIGHT} up to {FPS} fps");
        Ok(Self {
            texture,
            view,
            blitter: surface_format.map(|format| wgpu::util::TextureBlitter::new(device, format)),
            buffer,
            pending: None,
            producer,
            failed: false,
        })
    }

    /// Poll the previous readback and enqueue the latest texture if possible.
    /// At most one readback and one queued CPU frame exist at any time.
    pub fn capture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
        if self.failed {
            return Ok(());
        }
        if let Err(error) = self.try_capture(device, queue) {
            self.failed = true;
            self.producer.stop();
            return Err(error);
        }
        Ok(())
    }

    fn try_capture(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<()> {
        if let Some(status) = self
            .producer
            .child
            .try_wait()
            .context("checking GStreamer")?
        {
            bail!("GStreamer exited with {status}");
        }
        device
            .poll(wgpu::PollType::Poll)
            .context("polling GPU readback")?;
        if let Some(pending) = &self.pending {
            match pending.try_recv() {
                Ok(Ok(())) => {
                    let mapped = self
                        .buffer
                        .slice(..)
                        .get_mapped_range()
                        .context("reading mapped GPU frame")?;
                    let frame = pack_rows(
                        &mapped,
                        ROW_BYTES as usize,
                        PADDED_ROW_BYTES as usize,
                        HEIGHT as usize,
                    );
                    drop(mapped);
                    self.buffer.unmap();
                    self.pending = None;
                    self.producer.send(frame)?;
                }
                Ok(Err(error)) => bail!("GPU readback failed: {error}"),
                Err(mpsc::TryRecvError::Empty) => return Ok(()),
                Err(mpsc::TryRecvError::Disconnected) => {
                    bail!("GPU readback callback disconnected")
                }
            }
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("PipeWire readback"),
        });
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(PADDED_ROW_BYTES),
                    rows_per_image: Some(HEIGHT),
                },
            },
            extent(),
        );
        queue.submit([encoder.finish()]);
        let (tx, rx) = mpsc::channel();
        self.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = tx.send(result);
            });
        self.pending = Some(rx);
        Ok(())
    }
}

fn extent() -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: WIDTH,
        height: HEIGHT,
        depth_or_array_layers: 1,
    }
}

fn pack_rows(bytes: &[u8], row: usize, stride: usize, height: usize) -> Vec<u8> {
    assert!(stride >= row && bytes.len() >= stride * height);
    if stride == row {
        return bytes[..row * height].to_vec();
    }
    let mut packed = Vec::with_capacity(row * height);
    for line in bytes.chunks_exact(stride).take(height) {
        packed.extend_from_slice(&line[..row]);
    }
    packed
}

struct Producer {
    child: Child,
    sender: Option<SyncSender<Vec<u8>>>,
    writer: Option<JoinHandle<()>>,
}

impl Producer {
    fn start() -> Result<Self> {
        let mut command = gstreamer_command();
        // A killed spectator cannot run Drop. Linux must also tear down its
        // PipeWire child on parent death, including when a consumer is absent.
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::process::CommandExt;
            let parent = std::process::id() as libc::pid_t;
            // SAFETY: this pre-exec closure only calls async-signal-safe libc
            // functions and does not access shared Rust state.
            unsafe {
                command.pre_exec(move || {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::getppid() != parent {
                        return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
                    }
                    Ok(())
                });
            }
        }
        let child = command
            .spawn()
            .context("starting gst-launch-1.0 (install GStreamer and its PipeWire plugin)")?;
        Self::from_child(child)
    }

    fn from_child(mut child: Child) -> Result<Self> {
        let mut stdin = child.stdin.take().context("opening GStreamer stdin")?;
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(1);
        let writer = thread::spawn(move || {
            for frame in rx {
                if let Err(error) = stdin.write_all(&frame) {
                    log::warn!("PipeWire video writer stopped: {error}");
                    break;
                }
            }
        });
        Ok(Self {
            child,
            sender: Some(tx),
            writer: Some(writer),
        })
    }

    fn send(&self, frame: Vec<u8>) -> Result<()> {
        if frame.len() != FRAME_BYTES {
            bail!("invalid video frame size: {}", frame.len());
        }
        match self
            .sender
            .as_ref()
            .context("producer already stopped")?
            .try_send(frame)
        {
            Ok(()) | Err(TrySendError::Full(_)) => Ok(()), // Drop rather than block rendering.
            Err(TrySendError::Disconnected(_)) => bail!("GStreamer video writer disconnected"),
        }
    }

    fn stop(&mut self) {
        // Kill first: a writer blocked in write_all must be released before join.
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.sender.take();
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn gstreamer_command() -> Command {
    let mut command = Command::new("gst-launch-1.0");
    command
        .args([
            "-q", "fdsrc", "fd=0", "blocksize=65536", "!", "rawvideoparse", "format=rgba",
        ])
        .arg(format!("width={WIDTH}"))
        .arg(format!("height={HEIGHT}"))
        .arg(format!("framerate={FPS}/1"))
        .args([
            "!",
            "pipewiresink", "mode=provide", "use-bufferpool=false", "sync=false",
            "stream-properties=props,media.class=Video/Source,media.type=Video,media.role=Production,node.name=tomori-spectator,node.description=Tomori-Spectator",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    command
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout_is_tightly_packed_rgba_4k() {
        assert_eq!((WIDTH, HEIGHT), (3840, 2160));
        assert_eq!(ROW_BYTES, 15_360);
        assert_eq!(PADDED_ROW_BYTES, ROW_BYTES);
        assert_eq!(FRAME_BYTES, 33_177_600);
        assert_eq!(FORMAT, wgpu::TextureFormat::Rgba8UnormSrgb);
    }

    #[test]
    fn gstreamer_format_matches_the_render_target() {
        let command = gstreamer_command();
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert!(args.contains(&"format=rgba"));
        assert!(args.contains(&"width=3840"));
        assert!(args.contains(&"height=2160"));
        assert!(args.contains(&"framerate=30/1"));
    }

    #[test]
    fn removes_gpu_row_padding_without_reordering_channels() {
        assert_eq!(
            pack_rows(&[1, 2, 3, 4, 99, 99, 5, 6, 7, 8, 99, 99], 4, 6, 2),
            vec![1, 2, 3, 4, 5, 6, 7, 8]
        );
    }

    #[test]
    fn bounded_channel_drops_when_full() {
        let (tx, _rx) = mpsc::sync_channel(1);
        assert!(tx.try_send(vec![1]).is_ok());
        assert!(matches!(tx.try_send(vec![2]), Err(TrySendError::Full(_))));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stop_reaps_child_even_when_writer_cannot_finish_a_frame() {
        let child = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let mut producer = Producer::from_child(child).unwrap();
        assert!(producer.send(vec![0; 1]).is_err());
        producer.send(vec![0; FRAME_BYTES]).unwrap();
        producer.stop();
        assert!(producer.child.try_wait().unwrap().is_some());
        assert!(producer.sender.is_none());
        assert!(producer.writer.is_none());
        assert!(producer.send(vec![0; FRAME_BYTES]).is_err());
    }
}
