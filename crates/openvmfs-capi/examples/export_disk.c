/*
 * Copies a virtual disk from a VMFS6 volume into a raw image file, reading only the ranges that hold data.
 *
 * Build:  cc -I crates/openvmfs-capi/include export_disk.c -L target/release -lopenvmfs_c -o export_disk
 * Run:    LD_LIBRARY_PATH=target/release ./export_disk /dev/mapper/snapshot-lun vm1/vm1.vmdk vm1.raw
 */
#define _FILE_OFFSET_BITS 64
#define _POSIX_C_SOURCE 200809L
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#include "openvmfs.h"

static int fail(const char *what, ovmfs_error *error) {
    /* Every failing call hands out an error object with a status and a message; the caller frees it. */
    fprintf(stderr, "%s: %s (status %d)\n", what, ovmfs_error_message(error), ovmfs_error_status(error));
    ovmfs_error_free(error);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s DEVICE DESCRIPTOR OUTPUT\n", argv[0]);
        return 2;
    }
    if (ovmfs_abi_version() != OVMFS_ABI_VERSION) {
        fprintf(stderr, "the library has interface version %u, this program was built for %u\n",
                ovmfs_abi_version(), (unsigned)OVMFS_ABI_VERSION);
        return 1;
    }

    ovmfs_error *error = NULL;
    ovmfs_volume *volume = NULL;
    if (ovmfs_volume_open(argv[1], 0, 0, &volume, &error) != OVMFS_OK) {
        return fail("cannot open the volume", error);
    }
    ovmfs_disk *disk = NULL;
    int32_t status = ovmfs_disk_open(volume, argv[2], &disk, &error);
    /* A disk stays usable after its volume handle is closed. */
    ovmfs_volume_close(volume);
    if (status == OVMFS_ERR_UNSUPPORTED || status == OVMFS_ERR_IN_USE) {
        /* The disk may be fine, but it cannot be read this way: use another transport for it. */
        return fail("this disk cannot be read from the volume", error);
    }
    if (status != OVMFS_OK) {
        return fail("cannot open the disk", error);
    }

    FILE *out = fopen(argv[3], "wb");
    if (out == NULL) {
        perror(argv[3]);
        ovmfs_disk_close(disk);
        return 1;
    }
    size_t count = ovmfs_disk_data_range_count(disk);
    ovmfs_range *ranges = calloc(count ? count : 1, sizeof *ranges);
    size_t buffer_size = 4u << 20;
    uint8_t *buffer = malloc(buffer_size);
    int result = 1;
    if (ranges != NULL && buffer != NULL) {
        count = ovmfs_disk_data_ranges(disk, ranges, count);
        result = 0;
        /* Everything outside the data ranges reads as zeros and is skipped. */
        for (size_t i = 0; i < count && result == 0; i++) {
            uint64_t offset = ranges[i].offset;
            uint64_t end = offset + ranges[i].length;
            while (offset < end && result == 0) {
                size_t wanted = end - offset < buffer_size ? (size_t)(end - offset) : buffer_size;
                if (ovmfs_disk_read(disk, offset, buffer, wanted, &error) != OVMFS_OK) {
                    result = fail("read failed", error);
                } else if (fseeko(out, (off_t)offset, SEEK_SET) != 0 || fwrite(buffer, 1, wanted, out) != wanted) {
                    perror("write failed");
                    result = 1;
                }
                offset += wanted;
            }
        }
        /* Give the output the full size of the disk, also when it ends with zeros. */
        uint64_t capacity = ovmfs_disk_capacity(disk);
        if (result == 0 && capacity > 0 && (fseeko(out, (off_t)capacity - 1, SEEK_SET) != 0 || fputc(0, out) == EOF)) {
            perror("write failed");
            result = 1;
        }
    }
    free(buffer);
    free(ranges);
    if (fclose(out) != 0) {
        result = 1;
    }
    ovmfs_disk_close(disk);
    return result;
}
