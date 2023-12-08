## Getting GDB to work on macOS with Apple's ARM chips

There appears to be no way to run GDB directly on macOS at this moment. 

In addition, LLDB does not work, because at this moment, LLDB does not recognize 
32-bit RISC-V architecture and would be unable to understand the frames and would 
uncontrollably keep single-stepping the program. It appears that generations of 
developers have tried to fill in this gap, but LLDB is just very different from GDB and is not a substitute. 

For these users, we recommend using a separate machine, such as a cloud instance, or a virtual machine to run GDB. 
People who opt in for macOS with ARM chips, including me myself, should have expected such situations to emerge one day.

Below we provide a way to get GDB running through QEMU. 

### Setting up a virtual machine for GDB via QEMU

We can start by creating an image.
```bash
(host) qemu-img create -f qcow2 gdb.img 10G
```

Then, download the Ubuntu Server ISO (https://ubuntu.com/download/server) and start a QEMU simulation for x86-64 with that ISO.
We recommend a recent one because we do need a recent version of `gdb-multiarch`, which would support riscv32.
```bash
(host) qemu-system-x86_64 -m 4096 \
     -drive file=gdb.img \
     -net user,hostfwd=tcp::10022-:22 \
     -net nic \
     -cdrom ./ubuntu-22.04.3-live-server-amd64.iso 
```

A window should pop up for the virtual machine. Follow the steps to install Ubuntu and remember to enable openssh server, 
as it can be useful to access the target through SSH (here, using port 10022).

After finishing the installation, quit the virtual machine and open the virtual machine again without the image.
```bash
(host) qemu-system-x86_64 -m 4096 \
     -drive file=gdb.img \
     -net user,hostfwd=tcp::10022-:22 \
     -net nic 
```

Then, we install `gdb-multiarch`. 
```bash
(target) sudo apt update
(target) sudo apt install gdb-multiarch
```

We can then enter GDB as usual (using `gdb-multiarch`, not `gdb`). 
To connect to the host, instead of using 127.0.0.1, use 10.0.2.2. If the address is different on 
your machine, you can install `net-tools` and look it up through `sudo ifconfig`.

```gdb
(gdb) tar rem 10.0.2.2:9000
```

