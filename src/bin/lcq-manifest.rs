//! `lcq-manifest` — what the fleet's administrator runs, and what a vessel
//! runs to check what it was given.
//!
//! A signed manifest (D27) needs somebody to sign it. That somebody is the
//! fleet's administrator, holding the one key that says who is in the fleet
//! (D26). This is the tool at both ends of that: it signs a manifest, it
//! checks one, and it prints the public half of a key in the form that goes
//! on a card for a vessel's crew to type in.
//!
//! ```text
//! lcq-manifest fingerprint --key admin.key
//! lcq-manifest sign        --key admin.key --in fleet.manifest --out fleet.signed
//! lcq-manifest verify      --admin-key admin.pub --in fleet.signed [--now <seconds>]
//! ```
//!
//! # The two key files are not the same thing
//!
//! `--key` is the administrator's **secret**: 32 bytes of hexadecimal, and the
//! only file here that has to be protected. It never goes to a vessel. The
//! tool refuses to read one that anybody but its owner can read.
//!
//! `--admin-key` is the **public** half, as a vessel holds it: the hand-typed
//! Crockford string this tool prints (`fingerprint`), which a vessel's crew
//! entered from a card. Verifying needs nothing secret.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use lcq::application::Manifest;
use lcq::wire::{SigningKey, VerifyingKey, hand};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    let options = match Options::read(args) {
        Ok(options) => options,
        Err(why) => return fail(&why),
    };

    match command.as_str() {
        "fingerprint" => fingerprint(&options),
        "sign" => sign(&options),
        "verify" => verify(&options),
        other => {
            if !other.is_empty() {
                eprintln!("lcq-manifest: {other:?} is not a command");
            }
            eprintln!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "\
usage:
  lcq-manifest fingerprint --key <secret>
  lcq-manifest sign        --key <secret> --in <manifest> [--out <file>]
  lcq-manifest verify      --admin-key <typed key> --in <manifest> [--now <seconds>]";

#[derive(Default)]
struct Options {
    key: Option<PathBuf>,
    admin_key: Option<PathBuf>,
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    now: Option<u64>,
}

impl Options {
    fn read(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut options = Self::default();
        let mut args = args.peekable();
        while let Some(flag) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{flag} wants a value"));
            match flag.as_str() {
                "--key" => options.key = Some(PathBuf::from(value()?)),
                "--admin-key" => options.admin_key = Some(PathBuf::from(value()?)),
                "--in" => options.input = Some(PathBuf::from(value()?)),
                "--out" => options.output = Some(PathBuf::from(value()?)),
                "--now" => {
                    let raw = value()?;
                    options.now = Some(
                        raw.parse()
                            .map_err(|_| format!("--now wants seconds, not {raw:?}"))?,
                    );
                }
                other => return Err(format!("{other:?} is not an option\n{USAGE}")),
            }
        }
        Ok(options)
    }
}

fn fail(why: &str) -> ExitCode {
    eprintln!("lcq-manifest: {why}");
    ExitCode::FAILURE
}

fn need<'a>(path: Option<&'a PathBuf>, flag: &str) -> Result<&'a Path, String> {
    path.map(PathBuf::as_path)
        .ok_or_else(|| format!("{flag} is required\n{USAGE}"))
}

/// Read the administrator's secret: 32 bytes of hexadecimal, from a file only
/// its owner can read.
fn read_secret(path: &Path) -> Result<SigningKey, String> {
    // The mode first: refusing after the bytes are already in this process is
    // a refusal that has already lost.
    refuse_if_readable(path)?;
    let text = fs::read_to_string(path).map_err(|why| format!("{}: {why}", path.display()))?;
    let trimmed = text.trim();
    let mut seed = [0; 32];
    if trimmed.len() != 64 {
        return Err(format!(
            "{}: an administration key is 32 bytes of hexadecimal, so 64 characters",
            path.display()
        ));
    }
    for (index, slot) in seed.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&trimmed[index * 2..index * 2 + 2], 16)
            .map_err(|_| format!("{}: not hexadecimal", path.display()))?;
    }
    Ok(SigningKey::from_seed(seed))
}

/// A secret that the group or the world can read is a secret that has already
/// left. Refusing is cheap; the alternative is a warning nobody reads.
#[cfg(unix)]
fn refuse_if_readable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path)
        .map_err(|why| format!("{}: {why}", path.display()))?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "{}: mode {:o} lets somebody else read the administration key; chmod 600 it",
            path.display(),
            mode & 0o777
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_if_readable(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// Read the public half the way a vessel holds it: the hand-typed string.
fn read_public(path: &Path) -> Result<VerifyingKey, String> {
    let text = fs::read_to_string(path).map_err(|why| format!("{}: {why}", path.display()))?;
    let bytes = hand::decode(&text).map_err(|why| format!("{}: {why}", path.display()))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| format!("{}: those are not a public key", path.display()))
}

fn fingerprint(options: &Options) -> ExitCode {
    let path = match need(options.key.as_ref(), "--key") {
        Ok(path) => path,
        Err(why) => return fail(&why),
    };
    let key = match read_secret(path) {
        Ok(key) => key,
        Err(why) => return fail(&why),
    };
    let public = key.verifying_key().to_bytes();
    println!("# Put this on the card. A vessel's crew types it in once.");
    println!("{}", hand::encode(&public));
    println!();
    println!("# And this is what the node prints back at every start.");
    println!("{}", hand::fingerprint(&public));
    ExitCode::SUCCESS
}

fn sign(options: &Options) -> ExitCode {
    let (key_path, input) = match (
        need(options.key.as_ref(), "--key"),
        need(options.input.as_ref(), "--in"),
    ) {
        (Ok(key), Ok(input)) => (key, input),
        (Err(why), _) | (_, Err(why)) => return fail(&why),
    };
    let key = match read_secret(key_path) {
        Ok(key) => key,
        Err(why) => return fail(&why),
    };
    let text = match fs::read_to_string(input) {
        Ok(text) => text,
        Err(why) => return fail(&format!("{}: {why}", input.display())),
    };
    let signed = match Manifest::sign_text(&text, &key) {
        Ok(signed) => signed,
        Err(why) => return fail(&format!("{}: {why}", input.display())),
    };

    if let Some(path) = options.output.as_ref() {
        if let Err(why) = fs::write(path, &signed) {
            return fail(&format!("{}: {why}", path.display()));
        }
        eprintln!("signed {} into {}", input.display(), path.display());
    } else if let Err(why) = std::io::stdout().write_all(signed.as_bytes()) {
        return fail(&format!("stdout: {why}"));
    }
    ExitCode::SUCCESS
}

fn verify(options: &Options) -> ExitCode {
    let (key_path, input) = match (
        need(options.admin_key.as_ref(), "--admin-key"),
        need(options.input.as_ref(), "--in"),
    ) {
        (Ok(key), Ok(input)) => (key, input),
        (Err(why), _) | (_, Err(why)) => return fail(&why),
    };
    let admin = match read_public(key_path) {
        Ok(key) => key,
        Err(why) => return fail(&why),
    };
    let text = match fs::read_to_string(input) {
        Ok(text) => text,
        Err(why) => return fail(&format!("{}: {why}", input.display())),
    };
    let manifest = match Manifest::parse(&text) {
        Ok(manifest) => manifest,
        Err(why) => return fail(&format!("{}: {why}", input.display())),
    };

    let now = options.now.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    });
    if let Err(why) = manifest.verify(&admin, now) {
        return fail(&format!("{}: {why}", input.display()));
    }

    println!("epoch {}", manifest.epoch());
    println!(
        "valid {} to {} (now {now})",
        manifest.valid_from(),
        manifest.valid_until()
    );
    // Not part of the canonical form, so anybody who can edit the file can
    // set it to anything without breaking the signature. Printed as the hint
    // it is, next to the fingerprint of the key that actually verified.
    println!("verified-by {}", hand::fingerprint(&admin.to_bytes()));
    println!("issuer-claims {} (unsigned)", manifest.issuer_fingerprint());
    println!("members {}", manifest.size());
    for member in manifest.members() {
        println!(
            "  {} {} {}",
            member.index(),
            member.competence(),
            member.id()
        );
    }
    let digest = manifest.digest();
    print!("manifest ");
    for byte in digest {
        print!("{byte:02x}");
    }
    println!();
    ExitCode::SUCCESS
}
