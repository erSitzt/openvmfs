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

#define OVMFS_ABI_VERSION 5

#define OVMFS_OK 0
#define OVMFS_ERR_IO 1            /* the device could not be read */
#define OVMFS_ERR_CORRUPT 2       /* the on-disk data contradicts the format */
#define OVMFS_ERR_UNSUPPORTED 3   /* not verified, refused: fall back */
#define OVMFS_ERR_NOT_FOUND 4
#define OVMFS_ERR_INVALID_INPUT 5
#define OVMFS_ERR_INTERNAL 6      /* a bug in the library */
#define OVMFS_ERR_IN_USE 7        /* a host was writing to the disk when the image was taken: snapshot the VM first */

/* Why something was refused with OVMFS_ERR_UNSUPPORTED (ovmfs_error_reason). More may be added; treat unknown
 * values like OVMFS_REASON_UNVERIFIED. */
#define OVMFS_REASON_NONE 0                    /* the error is not OVMFS_ERR_UNSUPPORTED */
#define OVMFS_REASON_UNVERIFIED 1              /* not verified against data written by ESXi */
#define OVMFS_REASON_RDM 2                     /* raw device mapping: the data lives on another device */
#define OVMFS_REASON_ENCRYPTED 3               /* encrypted, or another I/O filter */
#define OVMFS_REASON_OTHER_VMFS_VERSION 4      /* VMFS3 or VMFS5 */
#define OVMFS_REASON_VMFS_L 5                  /* VMFS-L, ESXi's own system partitions */
#define OVMFS_REASON_LEGACY_SPARSE 6           /* snapshot delta in the older COWD format */
#define OVMFS_REASON_CHAIN_LEAVES_DATASTORE 7  /* a snapshot chain or extent points outside the datastore */

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

#define OVMFS_BACKING_ZERO 0   /* the range reads as zeros */
#define OVMFS_BACKING_DEVICE 1 /* the bytes are on a device */

/* A range of a virtual disk and where its bytes are stored. */
typedef struct ovmfs_mapped_range {
    uint64_t offset;        /* on the virtual disk */
    uint64_t length;
    uint64_t device_offset; /* from the start of the device; 0 for OVMFS_BACKING_ZERO */
    uint32_t device;        /* position of the device in the list the volume was opened with; 0 for one device */
    uint32_t backing;       /* OVMFS_BACKING_* */
} ovmfs_mapped_range;

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

/*
 * A device that is not a path: the library reads it through a function of the caller. Useful for a LUN that is
 * reached through a tunnel, for an image in memory, or for tests.
 *
 * ovmfs_read_fn fills buffer with length bytes at offset and returns 0, or returns anything else on failure. It is
 * called from whatever thread uses the volume or its disks, from several at once.
 * ovmfs_release_fn, if not NULL, is called exactly once when the library no longer uses the device: when the open
 * fails, or when the volume and every disk opened from it have been closed. (If read is NULL, nothing is called.)
 * size is the size of the device in bytes. OVMFS_OPEN_DIRECT has no effect here.
 */
typedef int32_t (*ovmfs_read_fn)(void *context, uint64_t offset, uint8_t *buffer, size_t length);
typedef void (*ovmfs_release_fn)(void *context);
int32_t ovmfs_volume_open_device(ovmfs_read_fn read, ovmfs_release_fn release, void *context, uint64_t size,
                                 uint32_t partition, uint32_t flags, ovmfs_volume **out_volume,
                                 ovmfs_error **out_error);

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

/*
 * Where the bytes of the disk are stored, for callers that want to read the data from the devices themselves and
 * leave only the metadata to this library. Describes the disk from offset on, for at most length bytes: writes up to
 * capacity entries to out_ranges and their number to out_count. The entries are contiguous and start at offset. If
 * they end before offset + length, call again from where they ended. Reading an OVMFS_BACKING_DEVICE range from its
 * device gives exactly what ovmfs_disk_read returns for it.
 * Fails with OVMFS_ERR_INVALID_INPUT on a volume opened with OVMFS_OPEN_REPLAY_JOURNAL.
 */
int32_t ovmfs_disk_map(const ovmfs_disk *disk, uint64_t offset, uint64_t length, ovmfs_mapped_range *out_ranges,
                       size_t capacity, size_t *out_count, ovmfs_error **out_error);

int32_t ovmfs_error_status(const ovmfs_error *error);
int32_t ovmfs_error_reason(const ovmfs_error *error); /* OVMFS_REASON_* */
const char *ovmfs_error_message(const ovmfs_error *error); /* valid until the error is freed */
void ovmfs_error_free(ovmfs_error *error);

#ifdef __cplusplus
}
#endif

#endif /* OPENVMFS_H */
