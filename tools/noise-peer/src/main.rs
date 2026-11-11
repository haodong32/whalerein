//! WhaleRein Noise XX interop peer.
//!
//! This tool performs the same `Noise_XX_25519_ChaChaPoly_SHA256` handshake
//! with the same prologue and HKDF key schedule as Core, so the Node DSH
//! implementation can pair against it. It speaks a small stdin/stdout line
//! protocol and never handles real WhaleRein payloads.
//!
//! Usage:
//!   noise-peer responder --prologue <string> --private-key <hex> [--channel-id <hex>]
//!   noise-peer initiator --prologue <string> --private-key <hex> [--channel-id <hex>]
//!
//! Line protocol:
//!   msg <hex>                       handshake message (both roles)
//!   ready <fingerprint> <hhash> <channel-id> <c2h> <h2c>
//!   seal <aad-hex> <plaintext-hex>  -> sealed <nonce-hex> <ciphertext-hex>
//!   open <aad-hex> <nonce-hex> <ciphertext-hex> -> opened <plaintext-hex>
//!   ping                            -> pong
//!   quit                            -> exit

use std::io::{BufRead, Write};

use anyhow::{anyhow, Context, Result};
use whalerein_core::testing::{
    derive_channel_keys, fingerprint, open_with_key, seal_with_key, NoiseSession, StaticIdentity,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Initiator,
    Responder,
}

impl Role {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "initiator" => Ok(Self::Initiator),
            "responder" => Ok(Self::Responder),
            other => Err(anyhow!("unknown role {other}")),
        }
    }
}

struct Options {
    role: Role,
    prologue: String,
    private_key: [u8; 32],
    channel_id: String,
}

fn main() -> Result<()> {
    let options = parse_args(std::env::args().skip(1).collect())?;
    run(options)
}

fn parse_args(args: Vec<String>) -> Result<Options> {
    let mut iter = args.into_iter();
    let role = Role::parse(&iter.next().ok_or_else(|| anyhow!("missing role"))?)?;
    let mut prologue = None;
    let mut private_key = None;
    let mut channel_id = None;
    while let Some(flag) = iter.next() {
        let value = iter
            .next()
            .ok_or_else(|| anyhow!("missing value for {flag}"))?;
        match flag.as_str() {
            "--prologue" => prologue = Some(value),
            "--private-key" => {
                let bytes = hex::decode(&value).context("private key must be hex")?;
                let key: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| anyhow!("private key must be 32 bytes"))?;
                private_key = Some(key);
            }
            "--channel-id" => channel_id = Some(value),
            other => return Err(anyhow!("unknown flag {other}")),
        }
    }
    let prologue = prologue.ok_or_else(|| anyhow!("missing --prologue"))?;
    if prologue.is_empty() {
        return Err(anyhow!("--prologue must not be empty"));
    }
    let private_key = private_key.ok_or_else(|| anyhow!("missing --private-key"))?;
    let channel_id = channel_id.unwrap_or_else(|| hex::encode(random_bytes(16)));
    if channel_id.is_empty() {
        return Err(anyhow!("--channel-id must not be empty"));
    }
    Ok(Options {
        role,
        prologue,
        private_key,
        channel_id,
    })
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    getrandom::fill(&mut bytes).expect("os randomness");
    bytes
}

fn run(options: Options) -> Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    run_with_io(options, stdin.lock(), &mut stdout)
}

fn run_with_io(options: Options, input: impl BufRead, mut stdout: impl Write) -> Result<()> {
    let identity = StaticIdentity::from_private_key(&options.private_key)
        .map_err(|_| anyhow!("invalid private key"))?;
    let mut session = match options.role {
        Role::Initiator => NoiseSession::initiator(&identity, &options.prologue),
        Role::Responder => NoiseSession::responder(&identity, &options.prologue),
    }
    .map_err(|_| anyhow!("noise init failed"))?;

    let lines = input.lines();

    if options.role == Role::Initiator {
        let message = session
            .write(b"whalerein-noise-peer")
            .map_err(|_| anyhow!("noise write failed"))?;
        writeln!(stdout, "msg {}", hex::encode(message))?;
        stdout.flush()?;
    }

    for line in lines {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_whitespace();
        let command = parts.next().unwrap_or_default();
        match command {
            "msg" => {
                let message = hex::decode(parts.next().unwrap_or_default())
                    .context("handshake message must be hex")?;
                let payload = session
                    .read(&message)
                    .map_err(|_| anyhow!("noise read failed"))?;
                let _ = payload;
                if session.is_finished() {
                    emit_ready(&mut stdout, &session, &options)?;
                } else if options.role == Role::Responder || options.role == Role::Initiator {
                    let response = session
                        .write(b"whalerein-noise-peer")
                        .map_err(|_| anyhow!("noise write failed"))?;
                    writeln!(stdout, "msg {}", hex::encode(response))?;
                    stdout.flush()?;
                    if session.is_finished() {
                        emit_ready(&mut stdout, &session, &options)?;
                    }
                }
            }
            "seal" => {
                let aad = hex::decode(parts.next().unwrap_or_default()).context("aad hex")?;
                let plaintext =
                    hex::decode(parts.next().unwrap_or_default()).context("plaintext hex")?;
                let keys = keys(&session, &options)?;
                let (nonce, ciphertext) = seal_with_key(&keys.h2c, &aad, &plaintext)
                    .map_err(|_| anyhow!("seal failed"))?;
                writeln!(
                    stdout,
                    "sealed {} {}",
                    hex::encode(nonce),
                    hex::encode(ciphertext)
                )?;
                stdout.flush()?;
            }
            "open" => {
                let aad = hex::decode(parts.next().unwrap_or_default()).context("aad hex")?;
                let nonce = hex::decode(parts.next().unwrap_or_default()).context("nonce hex")?;
                let ciphertext =
                    hex::decode(parts.next().unwrap_or_default()).context("ciphertext hex")?;
                let keys = keys(&session, &options)?;
                match open_with_key(&keys.c2h, &aad, &nonce, &ciphertext) {
                    Ok(plaintext) => {
                        writeln!(stdout, "opened {}", hex::encode(plaintext))?;
                    }
                    Err(_) => {
                        writeln!(stdout, "error authentication_failed")?;
                    }
                }
                stdout.flush()?;
            }
            "ping" => {
                writeln!(stdout, "pong")?;
                stdout.flush()?;
            }
            "quit" => return Ok(()),
            other => {
                writeln!(stdout, "error unknown_command {other}")?;
                stdout.flush()?;
            }
        }
    }
    Ok(())
}

fn keys(session: &NoiseSession, options: &Options) -> Result<whalerein_core::testing::ChannelKeys> {
    let hash = session
        .handshake_hash()
        .map_err(|_| anyhow!("handshake not complete"))?;
    derive_channel_keys(&hash, &options.prologue).map_err(|_| anyhow!("key derivation failed"))
}

fn emit_ready(stdout: &mut impl Write, session: &NoiseSession, options: &Options) -> Result<()> {
    let hash = session
        .handshake_hash()
        .map_err(|_| anyhow!("handshake not complete"))?;
    let keys = derive_channel_keys(&hash, &options.prologue)
        .map_err(|_| anyhow!("key derivation failed"))?;
    let remote = session
        .remote_static()
        .ok_or_else(|| anyhow!("remote static key missing"))?;
    writeln!(
        stdout,
        "ready {} {} {} {} {}",
        fingerprint(&remote),
        hex::encode(hash),
        options.channel_id,
        hex::encode(keys.c2h),
        hex::encode(keys.h2c)
    )?;
    stdout.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn parses_valid_noise_responder_options() {
        let options = parse_args(args(&[
            "responder",
            "--prologue",
            "p",
            "--private-key",
            &"11".repeat(32),
            "--channel-id",
            "chan",
        ]))
        .unwrap();
        assert_eq!(options.role, Role::Responder);
        assert_eq!(options.prologue, "p");
        assert_eq!(options.private_key, [0x11; 32]);
        assert_eq!(options.channel_id, "chan");
    }

    #[test]
    fn generates_channel_id_when_absent() {
        let options = parse_args(args(&[
            "initiator",
            "--prologue",
            "p",
            "--private-key",
            &"22".repeat(32),
        ]))
        .unwrap();
        assert_eq!(options.channel_id.len(), 32);
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(parse_args(args(&[])).is_err());
        assert!(parse_args(args(&["bogus"])).is_err());
        assert!(parse_args(args(&["responder", "--prologue"])).is_err());
        assert!(parse_args(args(&[
            "responder",
            "--prologue",
            "p",
            "--private-key",
            "00"
        ]))
        .is_err());
        assert!(parse_args(args(&[
            "responder",
            "--prologue",
            "p",
            "--private-key",
            &"00".repeat(32),
            "--channel-id",
            ""
        ]))
        .is_err());
        assert!(parse_args(args(&[
            "responder",
            "--prologue",
            "p",
            "--private-key",
            &"00".repeat(32),
            "--unknown",
            "x"
        ]))
        .is_err());
    }

    #[test]
    fn completes_noise_handshake_and_frame_exchange_between_roles() {
        let host = StaticIdentity::generate();
        let device = StaticIdentity::generate();
        let prologue = "whalerein/v1/channel|h|d|nego";
        let mut initiator = NoiseSession::initiator(&device, prologue).unwrap();
        let mut responder = NoiseSession::responder(&host, prologue).unwrap();

        let msg1 = initiator.write(b"hello").unwrap();
        assert_eq!(responder.read(&msg1).unwrap(), b"hello");
        let msg2 = responder.write(b"ready").unwrap();
        assert_eq!(initiator.read(&msg2).unwrap(), b"ready");
        let msg3 = initiator.write(b"").unwrap();
        assert_eq!(responder.read(&msg3).unwrap(), b"");

        let client_keys =
            derive_channel_keys(&initiator.handshake_hash().unwrap(), prologue).unwrap();
        let host_keys =
            derive_channel_keys(&responder.handshake_hash().unwrap(), prologue).unwrap();
        assert_eq!(client_keys.c2h, host_keys.c2h);
        assert_eq!(client_keys.h2c, host_keys.h2c);

        let (nonce, ciphertext) = seal_with_key(&client_keys.c2h, b"aad", b"data").unwrap();
        assert_eq!(
            open_with_key(&host_keys.c2h, b"aad", &nonce, &ciphertext).unwrap(),
            b"data"
        );
    }

    fn fixture_options(role: Role) -> Options {
        Options {
            role,
            prologue: "whalerein-noise-unit-fixture".into(),
            private_key: [0x11; 32],
            channel_id: "fixture-channel".into(),
        }
    }

    #[test]
    fn blank_lines_ping_unknown_commands_quit_and_eof_follow_the_line_protocol() {
        let mut output = Vec::new();
        run_with_io(
            fixture_options(Role::Responder),
            std::io::Cursor::new(b"\n   \nping\nunknown_fixture\nquit\nping\n"),
            &mut output,
        )
        .unwrap();
        assert!(output == b"pong\nerror unknown_command unknown_fixture\n");
        let mut output = Vec::new();
        run_with_io(
            fixture_options(Role::Responder),
            std::io::Cursor::new(b"ping\n"),
            &mut output,
        )
        .unwrap();
        assert!(output == b"pong\n");
    }

    #[test]
    fn malformed_line_arguments_and_use_before_handshake_return_explicit_errors() {
        for input in [
            "msg not-hex\n",
            "msg 00\n",
            "seal zz 00\n",
            "seal 00 zz\n",
            "seal 00 00\n",
            "open zz 00 00\n",
            "open 00 zz 00\n",
            "open 00 00 zz\n",
            "open 00 00 00\n",
        ] {
            assert!(run_with_io(
                fixture_options(Role::Responder),
                std::io::Cursor::new(input),
                Vec::new()
            )
            .is_err());
        }
        assert!(run_with_io(
            fixture_options(Role::Responder),
            std::io::Cursor::new([255, b'\n']),
            Vec::new()
        )
        .is_err());
        let options = fixture_options(Role::Responder);
        let identity = StaticIdentity::from_private_key(&options.private_key).unwrap();
        let incomplete = NoiseSession::responder(&identity, &options.prologue).unwrap();
        assert!(emit_ready(&mut Vec::new(), &incomplete, &options).is_err());
    }

    #[test]
    fn output_write_and_flush_failures_propagate_without_claiming_success() {
        struct BrokenWriter(bool);
        impl Write for BrokenWriter {
            fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
                if self.0 {
                    Ok(value.len())
                } else {
                    Err(std::io::Error::other("fixture write failure"))
                }
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Err(std::io::Error::other("fixture flush failure"))
            }
        }
        for role in [Role::Initiator, Role::Responder] {
            for writes_succeed in [false, true] {
                assert!(run_with_io(
                    fixture_options(role),
                    std::io::Cursor::new(b"ping\n"),
                    BrokenWriter(writes_succeed)
                )
                .is_err());
            }
        }
    }

    #[test]
    fn missing_empty_or_non_hex_required_options_are_rejected() {
        for values in [
            vec!["responder", "--private-key", "11"],
            vec!["responder", "--prologue", ""],
            vec!["responder", "--prologue", "p"],
            vec!["responder", "--prologue", "p", "--private-key", "zz"],
        ] {
            assert!(parse_args(args(&values)).is_err());
        }
    }

    #[cfg(unix)]
    fn exercise_line_handshake(role: Role) {
        use std::io::BufReader;
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        fn line(reader: &mut impl BufRead) -> String {
            let mut value = String::new();
            assert!(
                reader.read_line(&mut value).unwrap() > 0,
                "fixture expected a response line"
            );
            value.trim_end().to_owned()
        }
        fn message(line: &str) -> Vec<u8> {
            let encoded = line.strip_prefix("msg ").expect("handshake line kind");
            hex::decode(encoded).expect("handshake hex")
        }

        let (tool, mut peer) = UnixStream::pair().unwrap();
        tool.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let tool_reader = BufReader::new(tool.try_clone().unwrap());
        let worker =
            std::thread::spawn(move || run_with_io(fixture_options(role), tool_reader, tool));
        let mut responses = BufReader::new(peer.try_clone().unwrap());
        let identity = StaticIdentity::from_private_key(&[0x22; 32]).unwrap();
        let options = fixture_options(role);
        let mut session = match role {
            Role::Initiator => NoiseSession::responder(&identity, &options.prologue).unwrap(),
            Role::Responder => NoiseSession::initiator(&identity, &options.prologue).unwrap(),
        };
        match role {
            Role::Responder => {
                let first = session.write(b"synthetic handshake").unwrap();
                writeln!(peer, "msg {}", hex::encode(first)).unwrap();
                assert!(
                    session.read(&message(&line(&mut responses))).unwrap()
                        == b"whalerein-noise-peer"
                );
                let finish = session.write(b"synthetic finish").unwrap();
                writeln!(peer, "msg {}", hex::encode(finish)).unwrap();
            }
            Role::Initiator => {
                assert!(
                    session.read(&message(&line(&mut responses))).unwrap()
                        == b"whalerein-noise-peer"
                );
                let second = session.write(b"synthetic handshake").unwrap();
                writeln!(peer, "msg {}", hex::encode(second)).unwrap();
                assert!(
                    session.read(&message(&line(&mut responses))).unwrap()
                        == b"whalerein-noise-peer"
                );
            }
        }
        let ready = line(&mut responses);
        let fields: Vec<_> = ready.split_whitespace().collect();
        assert!(fields.len() == 6 && fields[0] == "ready");
        let hash = session.handshake_hash().unwrap();
        let derived = derive_channel_keys(&hash, &options.prologue).unwrap();
        assert!(fields[1] == fingerprint(identity.public_key()));
        assert!(fields[2] == hex::encode(hash));
        assert!(fields[3] == options.channel_id);
        assert!(fields[4] == hex::encode(derived.c2h));
        assert!(fields[5] == hex::encode(derived.h2c));

        let aad = b"fixture aad";
        let plaintext = b"synthetic fixture bytes";
        writeln!(peer, "seal {} {}", hex::encode(aad), hex::encode(plaintext)).unwrap();
        let sealed = line(&mut responses);
        let parts: Vec<_> = sealed.split_whitespace().collect();
        assert!(parts.len() == 3 && parts[0] == "sealed");
        let nonce = hex::decode(parts[1]).unwrap();
        let ciphertext = hex::decode(parts[2]).unwrap();
        assert!(nonce.len() == 12);
        assert!(open_with_key(&derived.h2c, aad, &nonce, &ciphertext).unwrap() == plaintext);

        let (nonce, ciphertext) = seal_with_key(&derived.c2h, aad, plaintext).unwrap();
        writeln!(
            peer,
            "open {} {} {}",
            hex::encode(aad),
            hex::encode(&nonce),
            hex::encode(&ciphertext)
        )
        .unwrap();
        let opened = line(&mut responses);
        assert!(opened == format!("opened {}", hex::encode(plaintext)));
        for (bad_aad, bad_nonce, bad_ciphertext) in [
            (b"wrong".as_slice(), nonce.clone(), ciphertext.clone()),
            (aad.as_slice(), vec![0], ciphertext.clone()),
            (aad.as_slice(), nonce, vec![0]),
        ] {
            writeln!(
                peer,
                "open {} {} {}",
                hex::encode(bad_aad),
                hex::encode(bad_nonce),
                hex::encode(bad_ciphertext)
            )
            .unwrap();
            assert!(line(&mut responses) == "error authentication_failed");
        }
        writeln!(peer, "ping").unwrap();
        assert!(line(&mut responses) == "pong");
        writeln!(peer, "quit").unwrap();
        assert!(worker.join().unwrap().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn responder_line_protocol_completes_handshake_and_authenticated_frames() {
        exercise_line_handshake(Role::Responder);
    }

    #[cfg(unix)]
    #[test]
    fn initiator_line_protocol_completes_handshake_and_authenticated_frames() {
        exercise_line_handshake(Role::Initiator);
    }
}
