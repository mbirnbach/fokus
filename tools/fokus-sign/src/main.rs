//! Signs the release downloads so the app's updater can tell they come from
//! our release builds (see packaging/UPDATES.md).
//!
//!   fokus-sign keygen <private key file>   new key pair; writes the private key there
//!                                          and the public key to fokus/update-key.pub
//!   fokus-sign sign <file>...              writes <file>.sig for each, with the private
//!                                          key from UPDATE_SIGNING_KEY (64 hex digits)

use std::path::Path;

use ed25519_dalek::{Signer, SigningKey, VerifyingKey};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("keygen") if args.len() == 2 => keygen(Path::new(&args[1])),
        Some("sign") if args.len() > 1 => sign(&args[1..]),
        _ => Err("usage: fokus-sign keygen <private key file> | fokus-sign sign <file>...".into()),
    };
    if let Err(e) = result {
        eprintln!("fokus-sign: {e}");
        std::process::exit(1);
    }
}

fn keygen(private: &Path) -> Result<(), String> {
    if private.exists() {
        return Err(format!("{} exists; not overwriting a key", private.display()));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| e.to_string())?;
    let key = SigningKey::from_bytes(&seed);
    write_private(private, &hex(&seed))?;
    let public = public_key_file();
    std::fs::write(&public, hex(key.verifying_key().as_bytes()) + "\n").map_err(|e| e.to_string())?;
    println!("Private key: {} (keep a backup; store it as the UPDATE_SIGNING_KEY secret)", private.display());
    println!("Public key:  {}", public.canonicalize().unwrap_or(public).display());
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn sign(files: &[String]) -> Result<(), String> {
    let text = std::env::var("UPDATE_SIGNING_KEY").map_err(|_| "UPDATE_SIGNING_KEY is not set")?;
    let seed: [u8; 32] = unhex(text.trim()).and_then(|b| b.try_into().ok()).ok_or("UPDATE_SIGNING_KEY is not 64 hex digits")?;
    let key = SigningKey::from_bytes(&seed);
    // The app only accepts updates signed with the key it carries; catch a mismatch here.
    let public = std::fs::read_to_string(public_key_file()).map_err(|e| format!("fokus/update-key.pub: {e}"))?;
    let public: [u8; 32] = unhex(public.trim()).and_then(|b| b.try_into().ok()).ok_or("fokus/update-key.pub is not 64 hex digits")?;
    if VerifyingKey::from_bytes(&public).ok() != Some(key.verifying_key()) {
        return Err("UPDATE_SIGNING_KEY doesn't belong to fokus/update-key.pub; the app would refuse these updates".into());
    }
    for file in files {
        let data = std::fs::read(file).map_err(|e| format!("{file}: {e}"))?;
        std::fs::write(format!("{file}.sig"), key.sign(&data).to_bytes()).map_err(|e| format!("{file}.sig: {e}"))?;
        println!("signed {file}");
    }
    Ok(())
}

fn public_key_file() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fokus/update-key.pub")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (s.len() % 2 == 0).then_some(())?;
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
