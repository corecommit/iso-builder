# 🧠 iso-builder

An interactive terminal application built in Rust that helps you create custom Linux ISOs without writing complex bash scripts.

## What it does

1. Scans your PC for existing `.iso` files.
2. Lets you select an ISO using your arrow keys.
3. Provides a visual menu with checkboxes to pick extra apps like Firefox or Docker, and system tweaks like enabling SSH.
4. Generates a ready-to-run bash script called `run-builder.sh`.
5. You run that script as root, and it extracts the ISO, injects your selected apps and tweaks, and spits out a brand new `custom-remaster.iso`.

## How to compile and run

### 1. Install Rust
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. Clone the repository

```bash
git clone https://github.com/corecommit/custom-linux-iso.git
cd custom-linux-iso
```

### 3. Compile and run

```bash
cargo run --release
```

### To just build the binary without running it:

```bash
cargo build --release
```
