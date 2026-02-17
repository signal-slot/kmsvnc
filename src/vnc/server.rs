use std::sync::Arc;

use anyhow::{bail, Context, Result};
use cipher::{BlockEncrypt, KeyInit};
use des::Des;
use rand::Rng;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};

use crate::frame_diff::{self, DirtyTiles};

/// Input event forwarded from VNC client to the input subsystem.
#[derive(Debug, Clone)]
pub enum InputEvent {
    Pointer { button_mask: u8, x: u16, y: u16 },
    Key { down: bool, keysym: u32 },
}

/// Client-negotiated pixel format.
#[derive(Clone, Debug)]
struct ClientPixelFormat {
    bpp: u8,
    big_endian: bool,
    red_max: u16,
    green_max: u16,
    blue_max: u16,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
}

impl ClientPixelFormat {
    /// Our server's default: 32bpp LE, red=16, green=8, blue=0 (BGRX byte order).
    fn server_default() -> Self {
        Self {
            bpp: 32,
            big_endian: false,
            red_max: 255,
            green_max: 255,
            blue_max: 255,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        }
    }

    fn from_bytes(buf: &[u8]) -> Self {
        Self {
            bpp: buf[0],
            // buf[1] = depth, buf[3] = true-colour (we assume true-colour)
            big_endian: buf[2] != 0,
            red_max: u16::from_be_bytes([buf[4], buf[5]]),
            green_max: u16::from_be_bytes([buf[6], buf[7]]),
            blue_max: u16::from_be_bytes([buf[8], buf[9]]),
            red_shift: buf[10],
            green_shift: buf[11],
            blue_shift: buf[12],
        }
    }

    fn matches_server_default(&self) -> bool {
        self.bpp == 32
            && !self.big_endian
            && self.red_max == 255
            && self.green_max == 255
            && self.blue_max == 255
            && self.red_shift == 16
            && self.green_shift == 8
            && self.blue_shift == 0
    }
}

/// Convert one row of BGRA pixel data to the client's requested pixel format.
/// Reuses `out` buffer to avoid per-row allocation.
fn convert_row_into(bgra_row: &[u8], pf: &ClientPixelFormat, out: &mut Vec<u8>) {
    let bytes_pp = (pf.bpp / 8) as usize;
    let num_pixels = bgra_row.len() / 4;
    out.clear();
    out.reserve(num_pixels * bytes_pp);

    for i in 0..num_pixels {
        let off = i * 4;
        let b = bgra_row[off] as u32;
        let g = bgra_row[off + 1] as u32;
        let r = bgra_row[off + 2] as u32;

        let rs = if pf.red_max == 255 {
            r
        } else {
            r * pf.red_max as u32 / 255
        };
        let gs = if pf.green_max == 255 {
            g
        } else {
            g * pf.green_max as u32 / 255
        };
        let bs = if pf.blue_max == 255 {
            b
        } else {
            b * pf.blue_max as u32 / 255
        };

        let pixel = (rs << pf.red_shift) | (gs << pf.green_shift) | (bs << pf.blue_shift);

        match bytes_pp {
            4 => {
                if pf.big_endian {
                    out.extend_from_slice(&pixel.to_be_bytes());
                } else {
                    out.extend_from_slice(&pixel.to_le_bytes());
                }
            }
            2 => {
                if pf.big_endian {
                    out.extend_from_slice(&(pixel as u16).to_be_bytes());
                } else {
                    out.extend_from_slice(&(pixel as u16).to_le_bytes());
                }
            }
            1 => {
                out.push(pixel as u8);
            }
            _ => {
                out.extend_from_slice(&{ pixel }.to_le_bytes()[..bytes_pp]);
            }
        }
    }
}

/// Server-side pixel format: 32bpp, depth 24, little-endian,
/// true-color, blue at bits 0-7, green at 8-15, red at 16-23.
/// This matches BGRA byte order in memory.
const PIXEL_FORMAT: [u8; 16] = [
    32, // bits-per-pixel
    24, // depth
    0,  // big-endian-flag (little-endian)
    1,  // true-colour-flag
    0, 255, // red-max (255)
    0, 255, // green-max (255)
    0, 255, // blue-max (255)
    16,  // red-shift
    8,   // green-shift
    0,   // blue-shift
    0, 0, 0, // padding
];

/// Compute the VNC DES response for a given password and 16-byte challenge.
///
/// VNC DES key derivation (VNC-specific):
/// 1. Password is truncated/zero-padded to 8 bytes
/// 2. Each byte's bit order is reversed (MSB <-> LSB)
/// 3. DES ECB encrypts the 16-byte challenge as two 8-byte blocks
fn vnc_des_auth(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key_bytes = [0u8; 8];
    for (i, &b) in password.as_bytes().iter().take(8).enumerate() {
        key_bytes[i] = b;
    }
    // Reverse bit order of each byte (VNC-specific quirk)
    for byte in &mut key_bytes {
        *byte = byte.reverse_bits();
    }

    let cipher = Des::new_from_slice(&key_bytes).expect("DES key is always 8 bytes");

    let mut result = [0u8; 16];
    result.copy_from_slice(challenge);

    let (block0, block1) = result.split_at_mut(8);
    cipher.encrypt_block(block0.into());
    cipher.encrypt_block(block1.into());

    result
}

/// Perform VNC Authentication (Type 2) challenge-response.
/// Returns Ok(true) if auth succeeded, Ok(false) if failed.
async fn perform_vnc_auth(stream: &mut TcpStream, password: &str) -> Result<bool> {
    let challenge: [u8; 16] = rand::rng().random();

    stream
        .write_all(&challenge)
        .await
        .context("send VNC auth challenge")?;

    let mut response = [0u8; 16];
    stream
        .read_exact(&mut response)
        .await
        .context("read VNC auth response")?;

    let expected = vnc_des_auth(password, &challenge);
    Ok(response == expected)
}

/// Handle a single VNC client connection.
pub async fn handle_client(
    mut stream: TcpStream,
    width: u16,
    height: u16,
    mut frame_rx: watch::Receiver<Arc<Vec<u8>>>,
    capture_req_tx: std::sync::mpsc::Sender<()>,
    input_tx: mpsc::Sender<InputEvent>,
    password: Option<&str>,
    dirty_tiles: Arc<DirtyTiles>,
) -> Result<()> {
    // === RFB Handshake ===

    stream
        .write_all(b"RFB 003.008\n")
        .await
        .context("send protocol version")?;

    let mut ver_buf = [0u8; 12];
    stream
        .read_exact(&mut ver_buf)
        .await
        .context("read client version")?;

    // Parse client version to determine the RFB minor version.
    // Format: "RFB 003.MMM\n"
    let rfb_minor = std::str::from_utf8(&ver_buf)
        .ok()
        .and_then(|s| s.get(8..11))
        .and_then(|m| m.parse::<u16>().ok())
        .unwrap_or(8);
    tracing::info!("Client requested RFB 003.{:03}", rfb_minor);

    match rfb_minor {
        // RFB 3.3 (and older): server dictates security type as u32, no SecurityResult.
        0..=6 => {
            if let Some(pw) = password {
                // Type 2: VNC Authentication
                stream
                    .write_all(&2u32.to_be_bytes())
                    .await
                    .context("send security type 2 (3.3)")?;
                if !perform_vnc_auth(&mut stream, pw).await? {
                    // RFB 3.3: no SecurityResult, just close the connection
                    bail!("VNC authentication failed");
                }
            } else {
                stream
                    .write_all(&1u32.to_be_bytes())
                    .await
                    .context("send security type (3.3)")?;
            }
        }
        // RFB 3.7: security type list + client selection, but no SecurityResult.
        7 => {
            if let Some(pw) = password {
                stream
                    .write_all(&[1, 2])
                    .await
                    .context("send security types (3.7)")?;

                let mut sec_type = [0u8; 1];
                stream
                    .read_exact(&mut sec_type)
                    .await
                    .context("read security type selection (3.7)")?;
                if sec_type[0] != 2 {
                    bail!("Client selected unsupported security type {}", sec_type[0]);
                }
                if !perform_vnc_auth(&mut stream, pw).await? {
                    bail!("VNC authentication failed");
                }
            } else {
                stream
                    .write_all(&[1, 1])
                    .await
                    .context("send security types (3.7)")?;

                let mut sec_type = [0u8; 1];
                stream
                    .read_exact(&mut sec_type)
                    .await
                    .context("read security type selection (3.7)")?;
                if sec_type[0] != 1 {
                    bail!("Client selected unsupported security type {}", sec_type[0]);
                }
            }
        }
        // RFB 3.8+: security type list + client selection + SecurityResult.
        _ => {
            if let Some(pw) = password {
                stream
                    .write_all(&[1, 2])
                    .await
                    .context("send security types")?;

                let mut sec_type = [0u8; 1];
                stream
                    .read_exact(&mut sec_type)
                    .await
                    .context("read security type selection")?;
                if sec_type[0] != 2 {
                    bail!("Client selected unsupported security type {}", sec_type[0]);
                }

                if perform_vnc_auth(&mut stream, pw).await? {
                    // SecurityResult: OK
                    stream
                        .write_all(&0u32.to_be_bytes())
                        .await
                        .context("send security result")?;
                } else {
                    // SecurityResult: Failed
                    stream
                        .write_all(&1u32.to_be_bytes())
                        .await
                        .context("send security result (failed)")?;
                    let reason = b"Authentication failed";
                    stream
                        .write_all(&(reason.len() as u32).to_be_bytes())
                        .await
                        .ok();
                    stream.write_all(reason).await.ok();
                    bail!("VNC authentication failed");
                }
            } else {
                stream
                    .write_all(&[1, 1])
                    .await
                    .context("send security types")?;

                let mut sec_type = [0u8; 1];
                stream
                    .read_exact(&mut sec_type)
                    .await
                    .context("read security type selection")?;
                if sec_type[0] != 1 {
                    bail!("Client selected unsupported security type {}", sec_type[0]);
                }

                // SecurityResult: OK
                stream
                    .write_all(&0u32.to_be_bytes())
                    .await
                    .context("send security result")?;
            }
        }
    }

    // ClientInit
    let mut client_init = [0u8; 1];
    stream
        .read_exact(&mut client_init)
        .await
        .context("read ClientInit")?;

    // ServerInit
    let name = b"kmsvnc";
    let mut server_init = Vec::with_capacity(24 + name.len());
    server_init.extend_from_slice(&width.to_be_bytes());
    server_init.extend_from_slice(&height.to_be_bytes());
    server_init.extend_from_slice(&PIXEL_FORMAT);
    server_init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    server_init.extend_from_slice(name);
    stream
        .write_all(&server_init)
        .await
        .context("send ServerInit")?;

    tracing::info!("VNC handshake complete ({}x{})", width, height);

    // === Message loop ===

    let (reader, writer) = stream.into_split();
    let mut writer = BufWriter::with_capacity(65536, writer);
    let (update_req_tx, mut update_req_rx) = mpsc::channel::<bool>(4);
    let (pf_tx, pf_rx) = watch::channel(ClientPixelFormat::server_default());

    let reader_handle = tokio::spawn(async move {
        let r = read_client_messages(reader, update_req_tx, input_tx, pf_tx).await;
        if let Err(e) = &r {
            tracing::debug!("Client reader ended: {e}");
        }
        r
    });

    let stride = width as usize * 4;

    // Reusable buffer for pixel format conversion
    let mut convert_buf = Vec::new();

    let writer_loop = async {
        loop {
            let incremental = match update_req_rx.recv().await {
                Some(v) => v,
                None => return Ok::<(), anyhow::Error>(()),
            };

            if incremental {
                // Request a capture and wait for a new frame
                let _ = capture_req_tx.send(());
                if frame_rx.changed().await.is_err() {
                    return Ok(());
                }
            }

            // Drain queued requests (coalesce)
            while update_req_rx.try_recv().is_ok() {}

            let frame = frame_rx.borrow_and_update().clone();

            let rects = if incremental {
                // Drain accumulated dirty tiles set by the capture thread
                let rects = dirty_tiles.drain_to_rects();
                if rects.is_empty() {
                    // Nothing changed — send empty FramebufferUpdate (0 rects)
                    // to satisfy the client's request per RFB protocol
                    writer.write_all(&[0, 0, 0, 0]).await.context("write empty fb")?;
                    writer.flush().await.ok();
                    continue;
                }
                rects
            } else {
                // Non-incremental: full frame
                dirty_tiles.drain_to_rects(); // clear any stale bits
                vec![frame_diff::DirtyRect {
                    x: 0,
                    y: 0,
                    width,
                    height,
                }]
            };

            // Get current client pixel format
            let pf = pf_rx.borrow().clone();
            let need_convert = !pf.matches_server_default();

            // Build FramebufferUpdate
            let num_rects = rects.len() as u16;
            let mut hdr = [0u8; 4];
            hdr[0] = 0; // type
            hdr[2..4].copy_from_slice(&num_rects.to_be_bytes());
            writer.write_all(&hdr).await.context("write fb header")?;

            for rect in &rects {
                let mut rhdr = [0u8; 12];
                rhdr[0..2].copy_from_slice(&rect.x.to_be_bytes());
                rhdr[2..4].copy_from_slice(&rect.y.to_be_bytes());
                rhdr[4..6].copy_from_slice(&rect.width.to_be_bytes());
                rhdr[6..8].copy_from_slice(&rect.height.to_be_bytes());
                rhdr[8..12].copy_from_slice(&0i32.to_be_bytes()); // Raw encoding
                writer.write_all(&rhdr).await.context("write rect header")?;

                // Write tile data directly from frame buffer, row by row
                for row in rect.y..rect.y + rect.height {
                    let start = row as usize * stride + rect.x as usize * 4;
                    let end = start + rect.width as usize * 4;
                    let bgra_row = &frame[start..end];

                    if need_convert {
                        convert_row_into(bgra_row, &pf, &mut convert_buf);
                        writer
                            .write_all(&convert_buf)
                            .await
                            .context("write rect data")?;
                    } else {
                        writer
                            .write_all(bgra_row)
                            .await
                            .context("write rect data")?;
                    }
                }
            }

            writer.flush().await.ok();
        }
    };

    tokio::select! {
        r = writer_loop => {
            r?;
        }
        r = reader_handle => {
            r??;
        }
    }

    Ok(())
}

async fn read_client_messages(
    mut reader: tokio::net::tcp::OwnedReadHalf,
    update_req_tx: mpsc::Sender<bool>,
    input_tx: mpsc::Sender<InputEvent>,
    pf_tx: watch::Sender<ClientPixelFormat>,
) -> Result<()> {
    loop {
        let mut msg_type = [0u8; 1];
        reader
            .read_exact(&mut msg_type)
            .await
            .context("read message type")?;

        match msg_type[0] {
            // SetPixelFormat
            0 => {
                let mut buf = [0u8; 19]; // 3 padding + 16 pixel format
                reader
                    .read_exact(&mut buf)
                    .await
                    .context("read SetPixelFormat")?;
                let pf = ClientPixelFormat::from_bytes(&buf[3..19]);
                tracing::info!(
                    "Client SetPixelFormat: {}bpp {}, r_shift={} g_shift={} b_shift={}, \
                     r_max={} g_max={} b_max={}",
                    pf.bpp,
                    if pf.big_endian { "BE" } else { "LE" },
                    pf.red_shift,
                    pf.green_shift,
                    pf.blue_shift,
                    pf.red_max,
                    pf.green_max,
                    pf.blue_max,
                );
                let _ = pf_tx.send(pf);
            }
            // SetEncodings
            2 => {
                let mut buf = [0u8; 3]; // 1 padding + 2 number-of-encodings
                reader
                    .read_exact(&mut buf)
                    .await
                    .context("read SetEncodings header")?;
                let num_enc = u16::from_be_bytes([buf[1], buf[2]]) as usize;
                let mut enc_buf = vec![0u8; num_enc * 4];
                reader
                    .read_exact(&mut enc_buf)
                    .await
                    .context("read SetEncodings body")?;
            }
            // FramebufferUpdateRequest
            3 => {
                let mut buf = [0u8; 9];
                reader
                    .read_exact(&mut buf)
                    .await
                    .context("read FramebufferUpdateRequest")?;
                let incremental = buf[0] != 0;
                let _ = update_req_tx.send(incremental).await;
            }
            // KeyEvent
            4 => {
                let mut buf = [0u8; 7];
                reader.read_exact(&mut buf).await.context("read KeyEvent")?;
                let down = buf[0] != 0;
                let keysym = u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]);
                let _ = input_tx.send(InputEvent::Key { down, keysym }).await;
            }
            // PointerEvent
            5 => {
                let mut buf = [0u8; 5];
                reader
                    .read_exact(&mut buf)
                    .await
                    .context("read PointerEvent")?;
                let button_mask = buf[0];
                let x = u16::from_be_bytes([buf[1], buf[2]]);
                let y = u16::from_be_bytes([buf[3], buf[4]]);
                let _ = input_tx
                    .send(InputEvent::Pointer { button_mask, x, y })
                    .await;
            }
            // ClientCutText
            6 => {
                let mut buf = [0u8; 7];
                reader
                    .read_exact(&mut buf)
                    .await
                    .context("read ClientCutText header")?;
                let len = u32::from_be_bytes([buf[3], buf[4], buf[5], buf[6]]) as usize;
                let mut text_buf = vec![0u8; len];
                reader
                    .read_exact(&mut text_buf)
                    .await
                    .context("read ClientCutText body")?;
            }
            other => {
                bail!("Unknown client message type: {other}");
            }
        }
    }
}
