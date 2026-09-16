use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{self, Read, Write},
    net::TcpStream,
    process::ExitCode,
    time::Duration,
};

const PROTOCOL_VERSION: u16 = 1;
const HARD_MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Deserialize, Serialize)]
struct Request {
    id: u64,
    operation: String,
    payload: Value,
    #[serde(default)]
    declared_resources: Vec<DeclaredResourceRequest>,
}

#[derive(Deserialize, Serialize)]
struct DeclaredResourceRequest {
    name: String,
    units: u64,
}

#[derive(Deserialize, Serialize)]
struct Response {
    id: u64,
    result: Value,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireMessage {
    Hello {
        version: u16,
        secret: HandshakeSecret,
    },
    Request {
        version: u16,
        request: Request,
    },
    Response {
        version: u16,
        response: Response,
    },
}

#[derive(Deserialize, Serialize)]
struct HandshakeSecret([u8; 32]);

#[allow(dead_code, reason = "included as a module by the sealed E2E target")]
fn main() -> ExitCode {
    fixture_main()
}

pub(crate) fn fixture_main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fake plugin failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let address = std::env::args()
        .nth(1)
        .ok_or_else(|| io::Error::other("missing enrolled address"))?;
    let behavior = std::env::args().nth(2).unwrap_or_default();
    let mut secret = [0_u8; 32];
    io::stdin().read_exact(&mut secret)?;

    if behavior == "linger-before-connect" {
        std::thread::sleep(Duration::from_secs(5));
    }

    if behavior == "race-wrong-peer" {
        let attack_address = address.clone();
        let _attack = std::thread::spawn(move || {
            let mut held_partial = TcpStream::connect(&attack_address).ok();
            if let Some(held_partial) = held_partial.as_mut() {
                let _ = held_partial.write_all(&100_u32.to_be_bytes());
                let _ = held_partial.write_all(b"x");
            }
            for _ in 0..4 {
                if let Ok(mut wrong) = TcpStream::connect(&attack_address) {
                    let _ = write_frame(
                        &mut wrong,
                        &WireMessage::Hello {
                            version: PROTOCOL_VERSION,
                            secret: HandshakeSecret([0_u8; 32]),
                        },
                    );
                    // Rejection may reset this connection before the forged
                    // frame is written. That must not affect the exact child.
                    let _ = write_frame(
                        &mut wrong,
                        &WireMessage::Response {
                            version: PROTOCOL_VERSION,
                            response: Response {
                                id: 1,
                                result: serde_json::json!("forged"),
                            },
                        },
                    );
                }
            }
            std::thread::sleep(Duration::from_secs(1));
            drop(held_partial);
        });
        std::thread::sleep(Duration::from_millis(10));
    }

    let mut stream = TcpStream::connect(address)?;
    if behavior == "malformed-hello" {
        stream.write_all(&3_u32.to_be_bytes())?;
        stream.write_all(b"bad")?;
        return Ok(());
    }
    write_frame(
        &mut stream,
        &WireMessage::Hello {
            version: if behavior == "wrong-version" {
                PROTOCOL_VERSION + 1
            } else {
                PROTOCOL_VERSION
            },
            secret: HandshakeSecret(secret),
        },
    )?;
    if matches!(behavior.as_str(), "wrong-version") {
        return Ok(());
    }

    loop {
        let body = match read_frame(&mut stream) {
            Ok(body) => body,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        };
        let WireMessage::Request { version, request } =
            serde_json::from_slice(&body).map_err(io::Error::other)?
        else {
            return Err(io::Error::other("unexpected wire message"));
        };
        match request.operation.as_str() {
            "crash" => return Err(io::Error::other("fixture crash")),
            "malformed" => {
                stream.write_all(&3_u32.to_be_bytes())?;
                stream.write_all(b"bad")?;
            }
            "oversize" => stream.write_all(&u32::MAX.to_be_bytes())?,
            "slow" => std::thread::sleep(Duration::from_secs(5)),
            "brief" => {
                std::thread::sleep(Duration::from_millis(10));
                write_frame(
                    &mut stream,
                    &WireMessage::Response {
                        version,
                        response: Response {
                            id: request.id,
                            result: request.payload,
                        },
                    },
                )?;
            }
            "drip" => {
                let frame = encode_frame(&WireMessage::Response {
                    version,
                    response: Response {
                        id: request.id,
                        result: request.payload,
                    },
                })?;
                for byte in frame {
                    stream.write_all(&[byte])?;
                    std::thread::sleep(Duration::from_millis(30));
                }
            }
            "noisy" => {
                let chunk = [b'x'; 8192];
                let mut output = io::stdout().lock();
                for _ in 0..128 {
                    output.write_all(&chunk)?;
                }
                return Err(io::Error::other("fixture output limit"));
            }
            _ => write_frame(
                &mut stream,
                &WireMessage::Response {
                    version,
                    response: Response {
                        id: request.id,
                        result: request.payload,
                    },
                },
            )?,
        }
    }
}

fn write_frame(stream: &mut impl Write, value: &WireMessage) -> io::Result<()> {
    stream.write_all(&encode_frame(value)?)
}

fn encode_frame(value: &WireMessage) -> io::Result<Vec<u8>> {
    let body = serde_json::to_vec(value).map_err(io::Error::other)?;
    let frame_length = 4_usize
        .checked_add(body.len())
        .ok_or_else(|| io::Error::other("fixture frame length overflow"))?;
    if body.is_empty() || frame_length > HARD_MAX_FRAME_BYTES {
        return Err(io::Error::other("fixture frame exceeds bound"));
    }
    let length = u32::try_from(body.len()).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(frame_length);
    frame.extend_from_slice(&length.to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

fn read_frame(stream: &mut impl Read) -> io::Result<Vec<u8>> {
    let mut header = [0_u8; 4];
    stream.read_exact(&mut header)?;
    let length = usize::try_from(u32::from_be_bytes(header)).map_err(io::Error::other)?;
    if length == 0 || 4_usize.saturating_add(length) > HARD_MAX_FRAME_BYTES {
        return Err(io::Error::other("fixture frame exceeds bound"));
    }
    let mut body = vec![0_u8; length];
    stream.read_exact(&mut body)?;
    Ok(body)
}
