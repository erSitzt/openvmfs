/* Exercises the C interface the way a C caller would. Usage: smoke IMAGE. Exit code 0 means every check passed. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "openvmfs.h"

#define CHECK(condition)                                                      \
    do {                                                                      \
        if (!(condition)) {                                                   \
            fprintf(stderr, "check failed at line %d: %s\n", __LINE__, #condition); \
            return 1;                                                         \
        }                                                                     \
    } while (0)

int main(int argc, char **argv) {
    if (argc != 2) return 2;
    CHECK(ovmfs_abi_version() == OVMFS_ABI_VERSION);

    ovmfs_error *error = NULL;
    ovmfs_volume *volume = NULL;

    /* A missing device reports an error object and leaves the handle NULL. */
    CHECK(ovmfs_volume_open("/nonexistent/device", 0, 0, &volume, &error) == OVMFS_ERR_IO);
    CHECK(volume == NULL && error != NULL);
    CHECK(ovmfs_error_status(error) == OVMFS_ERR_IO);
    CHECK(strlen(ovmfs_error_message(error)) > 0);
    ovmfs_error_free(error);
    error = NULL;
    /* Errors are optional for the caller. */
    CHECK(ovmfs_volume_open("/nonexistent/device", 0, 0, &volume, NULL) == OVMFS_ERR_IO);
    CHECK(ovmfs_volume_open(argv[1], 0, 0x80, &volume, NULL) == OVMFS_ERR_INVALID_INPUT);

    /* A list of one device is the same as a single device; an empty list is refused. */
    const char *devices[1] = {argv[1]};
    CHECK(ovmfs_volume_open_devices(devices, 0, 0, 0, &volume, NULL) == OVMFS_ERR_INVALID_INPUT);
    CHECK(ovmfs_volume_open_devices(devices, 1, 0, 0, &volume, &error) == OVMFS_OK);
    CHECK(volume != NULL && error == NULL);
    ovmfs_volume_close(volume);
    volume = NULL;

    CHECK(ovmfs_volume_open(argv[1], 0, 0, &volume, &error) == OVMFS_OK);
    CHECK(volume != NULL && error == NULL);

    ovmfs_volume_info info;
    CHECK(ovmfs_volume_get_info(volume, &info, &error) == OVMFS_OK);
    CHECK(strcmp((const char *)info.label, "vmfs6") == 0);
    CHECK(info.capacity == 2147483648ull);
    CHECK(info.minor_version == 82);

    ovmfs_disk *disk = NULL;
    CHECK(ovmfs_disk_open(volume, "missing.vmdk", &disk, &error) == OVMFS_ERR_NOT_FOUND);
    CHECK(disk == NULL);
    ovmfs_error_free(error);
    error = NULL;
    CHECK(ovmfs_disk_open(volume, "rdm.vmdk", &disk, &error) == OVMFS_ERR_UNSUPPORTED);
    ovmfs_error_free(error);
    error = NULL;

    CHECK(ovmfs_disk_open(volume, "normal.vmdk", &disk, &error) == OVMFS_OK);
    /* The disk keeps the volume alive. */
    ovmfs_volume_close(volume);

    CHECK(ovmfs_disk_capacity(disk) == 69ull << 30);
    CHECK(ovmfs_disk_data_range_count(disk) == 1);
    ovmfs_range ranges[4];
    CHECK(ovmfs_disk_data_ranges(disk, ranges, 4) == 1);
    CHECK(ranges[0].offset == 42ull << 30 && ranges[0].length == 1u << 20);
    CHECK(ovmfs_disk_data_ranges(disk, NULL, 0) == 0);

    size_t length = 1u << 20;
    uint8_t *buffer = malloc(length);
    CHECK(buffer != NULL);
    CHECK(ovmfs_disk_read(disk, ranges[0].offset, buffer, length, &error) == OVMFS_OK);
    unsigned long long sum = 0;
    for (size_t i = 0; i < length; i++) sum += buffer[i];
    memset(buffer, 0xAA, length);
    CHECK(ovmfs_disk_read(disk, 0, buffer, length, &error) == OVMFS_OK);
    for (size_t i = 0; i < length; i++) CHECK(buffer[i] == 0);

    /* A read of nothing succeeds without a buffer; a read beyond the end fails. */
    CHECK(ovmfs_disk_read(disk, 0, NULL, 0, &error) == OVMFS_OK);
    CHECK(ovmfs_disk_read(disk, ovmfs_disk_capacity(disk), buffer, 1, &error) == OVMFS_ERR_INVALID_INPUT);
    ovmfs_error_free(error);
    CHECK(ovmfs_disk_read(NULL, 0, buffer, 1, NULL) == OVMFS_ERR_INVALID_INPUT);

    free(buffer);
    ovmfs_disk_close(disk);
    ovmfs_disk_close(NULL);
    ovmfs_volume_close(NULL);
    ovmfs_error_free(NULL);
    printf("OK data-byte-sum=%llu\n", sum);
    return 0;
}
