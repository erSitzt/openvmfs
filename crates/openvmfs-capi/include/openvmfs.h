/*
 * OpenVmfs: read-only access to VMware VMFS6 volumes and the virtual disks stored on them.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Rules of this interface:
 *
 *  - Handles are opaque. The caller owns them and releases them with the matching _close / _free function.
 *    A disk stays valid after its volume has been closed.
 *  - Every fallible function returns a status. On failure it stores an error object in *out_error, unless
 *    out_error is NULL. The caller frees it with ovmfs_error_free. There is no global error state.
 *  - ovmfs_disk_read may be called from many threads at once on the same disk. Closing a handle while another
 *    thread uses it is not allowed.
 *  - OVMFS_ERR_UNSUPPORTED means: the data may be fine, but reading it would be a guess. Fall back to another
 *    way of reading it.
 *  - Strings are NUL-terminated UTF-8. Paths inside the datastore use '/' and are relative to its root.
 */
#ifndef OPENVMFS_H
#define OPENVMFS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define OVMFS_ABI_VERSION 2

#define OVMFS_OK 0
#define OVMFS_ERR_IO 1            /* the device could not be read */
#define OVMFS_ERR_CORRUPT 2       /* the on-disk data contradicts the format */
#define OVMFS_ERR_UNSUPPORTED 3   /* not verified, refused: fall back */
#define OVMFS_ERR_NOT_FOUND 4
#define OVMFS_ERR_INVALID_INPUT 5
#define OVMFS_ERR_INTERNAL 6      /* a bug in the library */
#define OVMFS_ERR_IN_USE 7        /* a host was writing to the disk when the image was taken: snapshot the VM first */

/* Read structures that have not been verified against ESXi instead of refusing them. For verification work. */
#define OVMFS_OPEN_ACCEPT_UNVERIFIED 1u
/* Read the devices without the page cache of the operating system (O_DIRECT, FILE_FLAG_NO_BUFFERING). */
#define OVMFS_OPEN_DIRECT 2u
/* Open disks although an ESXi host had them open for writing when the image was taken. Their content is then a mix
 * of old and new that no crash would produce; only for looking at data that is known to be damaged anyway. */
#define OVMFS_OPEN_ACCEPT_IN_USE 4u
/* Experimental: read the volume as if the transactions waiting in its journals had reached their final place, as
 * an ESXi host does after a crash. Disks that were open for writing can then be opened. Anything in a journal that
 * is not understood makes the open fail with OVMFS_ERR_UNSUPPORTED. */
#define OVMFS_OPEN_REPLAY_JOURNAL 8u

typedef struct ovmfs_volume ovmfs_volume;
typedef struct ovmfs_disk ovmfs_disk;
typedef struct ovmfs_error ovmfs_error;

typedef struct ovmfs_range {
    uint64_t offset;
    uint64_t length;
} ovmfs_range;

typedef struct ovmfs_volume_info {
    uint64_t capacity;          /* size of the logical volume in bytes */
    uint64_t file_block_size;
    uint64_t partition_offset;  /* offset of the VMFS partition on the opened device */
    uint32_t active_heartbeats; /* hosts holding locks at capture time; 0 does not prove an unmount */
    uint32_t minor_version;     /* 82 for VMFS 6.82 */
    uint8_t uuid[16];           /* datastore UUID as stored on disk */
    uint8_t label[129];         /* NUL-terminated UTF-8 */
} ovmfs_volume_info;

/* Version of this interface; compare with OVMFS_ABI_VERSION. */
uint32_t ovmfs_abi_version(void);

/*
 * Opens the VMFS6 volume on a block device or in an image file (opened read-only).
 * device_path: a whole disk with a GUID partition table, or the VMFS partition itself.
 * partition:   0 to detect the VMFS partition, otherwise its number counted from 1.
 * flags:       0, or OVMFS_OPEN_* flags combined with |.
 */
int32_t ovmfs_volume_open(const char *device_path, uint32_t partition, uint32_t flags,
                          ovmfs_volume **out_volume, ovmfs_error **out_error);
/*
 * Opens a VMFS6 volume that is stored on several devices (a datastore that was extended onto further LUNs).
 * device_paths: all devices of the datastore, in any order; each as for ovmfs_volume_open.
 * If one is missing, the call fails with OVMFS_ERR_INVALID_INPUT and a message that names the devices as ESXi
 * recorded them.
 */
int32_t ovmfs_volume_open_devices(const char *const *device_paths, size_t device_count, uint32_t partition,
                                  uint32_t flags, ovmfs_volume **out_volume, ovmfs_error **out_error);
void ovmfs_volume_close(ovmfs_volume *volume);
int32_t ovmfs_volume_get_info(const ovmfs_volume *volume, ovmfs_volume_info *out_info, ovmfs_error **out_error);

/* Opens the virtual disk described by the descriptor (the small .vmdk text file), for example "vm1/vm1.vmdk". */
int32_t ovmfs_disk_open(const ovmfs_volume *volume, const char *descriptor_path,
                        ovmfs_disk **out_disk, ovmfs_error **out_error);
void ovmfs_disk_close(ovmfs_disk *disk);

/* Size of the disk in bytes. */
uint64_t ovmfs_disk_capacity(const ovmfs_disk *disk);

/*
 * Fills buffer with length bytes of the disk at offset, or fails; never a short read.
 * Unallocated and never-written ranges read as zeros.
 */
int32_t ovmfs_disk_read(const ovmfs_disk *disk, uint64_t offset, uint8_t *buffer, size_t length,
                        ovmfs_error **out_error);

/* The ranges of the disk that hold data, sorted by offset. Everything outside them reads as zeros. */
size_t ovmfs_disk_data_range_count(const ovmfs_disk *disk);
size_t ovmfs_disk_data_ranges(const ovmfs_disk *disk, ovmfs_range *out_ranges, size_t capacity);

int32_t ovmfs_error_status(const ovmfs_error *error);
const char *ovmfs_error_message(const ovmfs_error *error); /* valid until the error is freed */
void ovmfs_error_free(ovmfs_error *error);

#ifdef __cplusplus
}
#endif

#endif /* OPENVMFS_H */
