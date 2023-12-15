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
```console
host@host:~$ qemu-img create -f qcow2 gdb.img 10G
```

Then, download the Ubuntu Server ISO (https://ubuntu.com/download/server) and start a QEMU simulation for x86-64 with that ISO.

It is important to use a very recent version of Ubuntu. For example, use 23.10 instead of 22.04 because we do need a recent version 
of `gdb-multiarch`, which would support riscv32 and is capable to demangle it (which is mangled by Rust).

```console
host@host:~$ qemu-system-x86_64 -m 4096 -drive file=gdb.img -net user,hostfwd=tcp::10022-:22 -net nic -cdrom ./ubuntu-22.04.3-live-server-amd64.iso 
```

A window should pop up for the virtual machine. Follow the steps to install Ubuntu and remember to enable openssh server, 
as it can be useful to access the target through SSH (here, using port 10022).

After finishing the installation, quit the virtual machine and open the virtual machine again without the image.
```console
host@host:~$ qemu-system-x86_64 -m 4096 -drive file=gdb.img -net user,hostfwd=tcp::10022-:22 -net nic 
```

Then, we install `gdb-multiarch`. 
```console
ubuntu@target:~$ sudo apt update
ubuntu@target:~$ sudo apt install gdb-multiarch
```

We can then enter GDB as usual (using `gdb-multiarch`, not `gdb`). 
To connect to the host, instead of using 127.0.0.1, use 10.0.2.2. If the address is different on 
your machine, you can install `net-tools` and look it up through `sudo ifconfig`.

```gdb
(gdb) tar rem 10.0.2.2:9000
```

### Enable the gdb-multiarch to read source files from the host

If the program is compiled with the debug information, we can further let GDB load the source files, 
so that we can see the files side-by-side. The problem is that our GDB is running within the guest virtual
machine and does not have access to those source code files.

We find a method from [here](https://superuser.com/questions/628169/how-to-share-a-directory-with-the-host-without-networking-in-qemu). 
This can be solved by attaching the host filesystem (the root /) to the guest, so that the guest can access the root.
```console
host@host:~$ qemu-system-x86_64 -m 4096 -drive file=gdb.img -net user,hostfwd=tcp::10022-:22 -net nic --virtfs local,path=/,security_model=none,mount_tag=hostshare
```

And in the guest, 
```console
ubuntu@target:~$ sudo mkdir /wherever
ubuntu@target:~$ sudo chmod 0777 /wherever
```

Then, `sudo vim` to edit `/etc/fstab` to include the following line.
```
hostshare   /wherever    9p      trans=virtio,version=9p2000.L   0 0
```

Refresh the information.
```console
ubuntu@target:~$ sudo mount -a
ubuntu@target:~$ sudo systemctl daemon-reload 
```

Then, in GDB, we tell GDB that the source files can be found at /wherever
```gdb
(gdb) set substitute-path / /wherever
```

Then, `layout split` should work if the program runs into the part where the source code is available.
The very beginning of an RISC Zero guest program would not have the source because the beginning is some
assembly. But, starting from the entry function, source codes can be found.
