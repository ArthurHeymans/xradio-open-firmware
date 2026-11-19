mod image;
#[cfg(test)]
mod image_tests;
mod stack;
use anyhow::{Context, Result, ensure};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};
fn build(root: &Path, target_dir: &Path, target: &str, bin: &str) -> Result<PathBuf> {
    let status = Command::new("rustup")
        .args([
            "run",
            "nightly",
            "cargo",
            "build",
            "--locked",
            "--release",
            "-p",
            "xr819-firmware",
            "--features",
            "xr819",
            "--bin",
            bin,
            "--target",
            target,
            "-Zbuild-std=core",
            "--target-dir",
        ])
        .arg(target_dir)
        .current_dir(root)
        .status()
        .context("running nightly Cargo")?;
    ensure!(status.success(), "build failed: {bin}");
    Ok(target_dir.join(target).join("release").join(bin))
}
fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let command = args.next();
    let check = match command.as_ref().and_then(|c| c.to_str()) {
        Some("image") => false,
        Some("check") => true,
        _ => anyhow::bail!("usage: cargo xtask image [output-directory] | cargo xtask check"),
    };
    ensure!(!check || args.len() == 0, "check takes no arguments");
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let target = env::current_dir()?.join(
        env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target")),
    );
    let out = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| target.join("images"));
    ensure!(args.next().is_none(), "too many arguments");
    let fw = build(root, &target, "thumbv5te-none-eabi", "hif-startup")?;
    if check {
        return stack::check(&fs::read(fw)?);
    }
    let fw = image::runtime(&fs::read(fw)?)?;
    let boot = build(
        root,
        &target,
        "armv5te-none-eabi",
        "download-boot-sectioned",
    )?;
    let boot = image::bootstrap(&fs::read(boot)?)?;
    fs::create_dir_all(&out)?;
    for (name, bytes) in [("boot_xr819.bin", boot), ("fw_xr819.bin", fw)] {
        let p = out.join(name);
        fs::write(&p, bytes)?;
        println!("{}", p.display());
    }
    Ok(())
}
