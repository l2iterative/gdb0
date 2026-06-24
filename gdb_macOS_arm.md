## Debugging on macOS with Apple Silicon

This page used to recommend running GDB inside an x86-64 Ubuntu VM because GDB
was hard to use directly on Apple Silicon. That is no longer the first thing to
try.

As of 2026, Homebrew provides
[`riscv64-elf-gdb`](https://formulae.brew.sh/formula/riscv64-elf-gdb) bottles
for Apple Silicon. This is the easiest GDB client for this repository on macOS:

```console
$ brew install riscv64-elf-gdb
$ cargo build
$ cargo run
```

In another terminal:

```console
$ riscv64-elf-gdb -q code
```

Then in GDB:

```gdb
(gdb) set architecture riscv:rv32
(gdb) set print asm-demangle on
(gdb) target remote 127.0.0.1:9000
```

Even though the package name says `riscv64`, this GDB can read the checked-in
`code` ELF as `elf32-littleriscv` once the architecture is set to
`riscv:rv32`.

You can verify the setup with:

```console
$ bash scripts/gdb-smoke.sh
```

The script starts `r0db`, connects `riscv64-elf-gdb`, single-steps once, and
checks that the instruction at `_start + 4` is decoded.

## LLDB status

LLDB is better than it was when this project was first written. Current Apple
LLDB can create a RISC-V 32-bit target:

```console
$ lldb -b \
    -o 'target create --arch riscv32 code' \
    -o 'image list' \
    -o 'quit'
```

On the local machine this reports the executable as `riscv32`.

Upstream context: LLDB has a RISC-V support tracking issue in
[`llvm-project`](https://github.com/llvm/llvm-project/issues/55383), documents
its [GDB remote protocol behavior](https://lldb.llvm.org/resources/lldbgdbremote.html),
and the [LLVM 20 release notes](https://releases.llvm.org/20.1.0/docs/ReleaseNotes.html)
mention additional RISC-V LLDB improvements.

That does not yet make LLDB a replacement for GDB here. LLDB can connect to the
current `r0db` GDB remote stub:

```lldb
(lldb) target create --arch riscv32 code
(lldb) gdb-remote 127.0.0.1:9000
```

but in local testing with Apple LLDB `2100.0.17.108`, it only reached a stopped
thread and did not provide usable register/frame inspection. For example,
`register read pc` failed after connection. Use `riscv64-elf-gdb` for normal
debugging; treat LLDB as experimental unless you are specifically improving the
stub's LLDB remote-protocol compatibility.

## Linux VM fallback

A Linux VM is still useful if:

- Homebrew's GDB package is not available on your macOS version.
- You need Ubuntu's `gdb-multiarch` specifically.
- You want to keep host debug tooling isolated from macOS.

The old QEMU route still works. Create a disk image:

```console
$ qemu-img create -f qcow2 gdb.img 10G
```

Download a current Ubuntu Server ISO from https://ubuntu.com/download/server,
then boot and install it:

```console
$ qemu-system-x86_64 \
    -m 4096 \
    -drive file=gdb.img \
    -net user,hostfwd=tcp::10022-:22 \
    -net nic \
    -cdrom ./ubuntu-<version>-live-server-amd64.iso
```

After installation, boot the VM without the ISO:

```console
$ qemu-system-x86_64 \
    -m 4096 \
    -drive file=gdb.img \
    -net user,hostfwd=tcp::10022-:22 \
    -net nic
```

Inside the VM:

```console
$ sudo apt update
$ sudo apt install gdb-multiarch
```

Because QEMU user networking exposes the macOS host at `10.0.2.2`, connect from
GDB inside the VM with:

```gdb
(gdb) set architecture riscv:rv32
(gdb) file code
(gdb) target remote 10.0.2.2:9000
```

## Reading source files from a VM

If the guest ELF contains debug information, GDB can show source files with
`layout split`, but the VM needs access to those paths. One simple QEMU option
is a 9p mount of the host filesystem:

```console
$ qemu-system-x86_64 \
    -m 4096 \
    -drive file=gdb.img \
    -net user,hostfwd=tcp::10022-:22 \
    -net nic \
    --virtfs local,path=/,security_model=none,mount_tag=hostshare
```

Inside the VM:

```console
$ sudo mkdir /host
$ sudo chmod 0777 /host
```

Add this line to `/etc/fstab`:

```fstab
hostshare   /host    9p      trans=virtio,version=9p2000.L   0 0
```

Then mount it:

```console
$ sudo mount -a
$ sudo systemctl daemon-reload
```

In GDB, map host paths to the mounted tree:

```gdb
(gdb) set substitute-path / /host
```

The very beginning of a RISC Zero guest may still be assembly without source
locations, but source should appear once execution reaches code compiled with
debug information.
