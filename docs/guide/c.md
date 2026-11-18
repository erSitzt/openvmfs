# Using the C interface

`openvmfs_c` is the library for every language that can call C. Its header is
[`crates/openvmfs-capi/include/openvmfs.h`](../../crates/openvmfs-capi/include/openvmfs.h), and a complete program
is in [`crates/openvmfs-capi/examples/export_disk.c`](../../crates/openvmfs-capi/examples/export_disk.c); the test
suite compiles and runs that program.

## Build and link

```bash
cargo build --release -p openvmfs-capi        # target/release/libopenvmfs_c.so, or openvmfs_c.dll on Windows
cc -I crates/openvmfs-capi/include app.c -L target/release -lopenvmfs_c -o app
```

## The shape of the interface

- **Handles.** `ovmfs_volume` and `ovmfs_disk` are opaque. You get them from an `open` function and give them back
  with the matching `close`. A disk stays usable after its volume is closed; handles may be closed in any order.
- **Status and error objects.** Every function that can fail returns a status, `OVMFS_OK` or `OVMFS_ERR_*`, and
  hands out an `ovmfs_error` with a message if you pass a place for it. You free it with `ovmfs_error_free`. There
  is no global or thread-local error state. Passing `NULL` for the error is allowed.
- **Threads.** `ovmfs_disk_read` may be called from many threads at once on one disk. Closing a handle while
  another thread is inside a call on it is not allowed.
- **No panics across the boundary.** An internal bug is reported as `OVMFS_ERR_INTERNAL`.
- **Version.** `ovmfs_abi_version()` returns the interface version of the library; compare it with
  `OVMFS_ABI_VERSION` from the header you compiled against.

## A minimal program

```c
#include "openvmfs.h"

ovmfs_error *error = NULL;
ovmfs_volume *volume = NULL;
if (ovmfs_volume_open("/dev/mapper/snapshot-lun", 0, OVMFS_OPEN_DIRECT, &volume, &error) != OVMFS_OK) {
    fprintf(stderr, "%s\n", ovmfs_error_message(error));
    ovmfs_error_free(error);
    return 1;
}

ovmfs_disk *disk = NULL;
int32_t status = ovmfs_disk_open(volume, "vm1/vm1.vmdk", &disk, &error);
ovmfs_volume_close(volume);
if (status != OVMFS_OK) { /* see the table below */ }

uint64_t capacity = ovmfs_disk_capacity(disk);
size_t count = ovmfs_disk_data_range_count(disk);
ovmfs_range *ranges = calloc(count, sizeof *ranges);
count = ovmfs_disk_data_ranges(disk, ranges, count);

uint8_t buffer[65536];
status = ovmfs_disk_read(disk, ranges[0].offset, buffer, sizeof buffer, &error);

ovmfs_disk_close(disk);
```

## Functions

| Function | Purpose |
|---|---|
| `ovmfs_volume_open(path, partition, flags, &volume, &error)` | Opens the datastore on one device or image. `partition` 0 detects the VMFS partition |
| `ovmfs_volume_open_devices(paths, count, partition, flags, &volume, &error)` | The same for a datastore on several devices; all are needed, in any order |
| `ovmfs_volume_get_info(volume, &info, &error)` | Label, UUID, capacity, version, number of hosts that were holding locks |
| `ovmfs_disk_open(volume, descriptor_path, &disk, &error)` | Opens a virtual disk by its descriptor (`vm1/vm1.vmdk`); follows snapshot chains |
| `ovmfs_disk_capacity(disk)` | Size of the disk in bytes |
| `ovmfs_disk_read(disk, offset, buffer, length, &error)` | Fills the buffer or fails; never a short read |
| `ovmfs_disk_data_range_count(disk)`, `ovmfs_disk_data_ranges(disk, out, capacity)` | The ranges that hold data; everything else reads as zeros |
| `ovmfs_error_status`, `ovmfs_error_message`, `ovmfs_error_free` | Reading and freeing an error |

Flags for the open functions, combined with `|`:

| Flag | Meaning |
|---|---|
| `OVMFS_OPEN_DIRECT` | Read past the operating system's page cache. Buffers need no alignment |
| `OVMFS_OPEN_REPLAY_JOURNAL` | Experimental: read the volume with its journals replayed |
| `OVMFS_OPEN_ACCEPT_IN_USE` | Open disks although a host was writing to them |
| `OVMFS_OPEN_ACCEPT_UNVERIFIED` | Read structures that were never verified against ESXi |

## Status codes

| Status | Meaning | What to do |
|---|---|---|
| `OVMFS_OK` | Success | |
| `OVMFS_ERR_IO` | The device could not be read | |
| `OVMFS_ERR_CORRUPT` | The data contradicts the format | Do not trust this object |
| `OVMFS_ERR_UNSUPPORTED` | Not verified, refused | Read the disk another way |
| `OVMFS_ERR_NOT_FOUND` | No such file, directory or volume | |
| `OVMFS_ERR_INVALID_INPUT` | A wrong argument, or a device missing from a datastore on several devices | |
| `OVMFS_ERR_INTERNAL` | A bug in the library | Please report it |
| `OVMFS_ERR_IN_USE` | A host was writing to the disk when the image was taken | Take a VM snapshot before the storage snapshot |

Treat unknown status values as errors: later versions may add some.

The interface covers volumes and virtual disks. Listing directories and reading plain files are in the Rust
library and in the command line tool, not yet here.
