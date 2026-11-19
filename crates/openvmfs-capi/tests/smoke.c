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

/* A device behind a read function: here simply a file, with counters to check the contract. */
struct file_device {
    FILE *file;
    int reads;
    int released;
    int fail;
};

static int32_t file_read(void *context, uint64_t offset, uint8_t *buffer, size_t length) {
    struct file_device *device = context;
    device->reads++;
    if (device->fail) return 5;
    if (fseek(device->file, (long)offset, SEEK_SET) != 0) return 1;
    return fread(buffer, 1, length, device->file) == length ? 0 : 2;
}

static void file_release(void *context) {
    ((struct file_device *)context)->released++;
}

static int check_caller_supplied_device(const char *image) {
    struct file_device device = {fopen(image, "rb"), 0, 0, 0};
    CHECK(device.file != NULL);
    CHECK(fseek(device.file, 0, SEEK_END) == 0);
    uint64_t size = (uint64_t)ftell(device.file);

    ovmfs_error *error = NULL;
    ovmfs_volume *volume = NULL;
    CHECK(ovmfs_volume_open_device(file_read, file_release, &device, size, 0, 0, &volume, &error) == OVMFS_OK);
    CHECK(device.reads > 0 && device.released == 0);
    ovmfs_volume_info info;
    CHECK(ovmfs_volume_get_info(volume, &info, &error) == OVMFS_OK);
    CHECK(strcmp((const char *)info.label, "vmfs6") == 0);

    ovmfs_disk *disk = NULL;
    CHECK(ovmfs_disk_open(volume, "normal.vmdk", &disk, &error) == OVMFS_OK);
    /* The device is still needed by the disk after the volume is closed ... */
    ovmfs_volume_close(volume);
    CHECK(device.released == 0);
    uint8_t sector[512];
    ovmfs_range range;
    CHECK(ovmfs_disk_data_ranges(disk, &range, 1) == 1);
    CHECK(ovmfs_disk_read(disk, range.offset, sector, sizeof sector, &error) == OVMFS_OK);
    /* ... a failing read function becomes an I/O error ... */
    device.fail = 1;
    CHECK(ovmfs_disk_read(disk, range.offset, sector, sizeof sector, &error) == OVMFS_ERR_IO);
    CHECK(strstr(ovmfs_error_message(error), "status 5") != NULL);
    ovmfs_error_free(error);
    error = NULL;
    /* ... and it is released exactly once, when the last handle goes. */
    ovmfs_disk_close(disk);
    CHECK(device.released == 1);

    /* A failed open releases the device too. */
    device.released = 0;
    CHECK(ovmfs_volume_open_device(file_read, file_release, &device, size, 0, 0, &volume, NULL) == OVMFS_ERR_IO);
    CHECK(volume == NULL && device.released == 1);
    CHECK(ovmfs_volume_open_device(NULL, file_release, &device, size, 0, 0, &volume, NULL) == OVMFS_ERR_INVALID_INPUT);
    CHECK(device.released == 1);
    fclose(device.file);
    return 0;
}

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

    /* The map: zeros, then the megabyte of data with its place on the device, then zeros again. */
    ovmfs_mapped_range mapped[4];
    size_t mapped_count = 99;
    CHECK(ovmfs_disk_map(disk, 0, ovmfs_disk_capacity(disk), mapped, 4, &mapped_count, &error) == OVMFS_OK);
    CHECK(mapped_count == 3);
    CHECK(mapped[0].offset == 0 && mapped[0].length == 42ull << 30 && mapped[0].backing == OVMFS_BACKING_ZERO);
    CHECK(mapped[1].offset == 42ull << 30 && mapped[1].length == 1u << 20);
    CHECK(mapped[1].backing == OVMFS_BACKING_DEVICE && mapped[1].device == 0);
    CHECK(mapped[2].offset + mapped[2].length == ovmfs_disk_capacity(disk));
    /* The bytes at that place on the device are the bytes of the disk. */
    FILE *raw = fopen(argv[1], "rb");
    CHECK(raw != NULL);
    uint8_t direct[512];
    CHECK(fseek(raw, (long)mapped[1].device_offset, SEEK_SET) == 0);
    CHECK(fread(direct, 1, sizeof direct, raw) == sizeof direct);
    fclose(raw);
    CHECK(ovmfs_disk_read(disk, mapped[1].offset, buffer, sizeof direct, &error) == OVMFS_OK);
    CHECK(memcmp(direct, buffer, sizeof direct) == 0);
    /* With room for one entry it stops after the first; a range outside the disk fails. */
    CHECK(ovmfs_disk_map(disk, 0, ovmfs_disk_capacity(disk), mapped, 1, &mapped_count, &error) == OVMFS_OK);
    CHECK(mapped_count == 1 && mapped[0].length == 42ull << 30);
    CHECK(ovmfs_disk_map(disk, 0, 0, NULL, 0, &mapped_count, &error) == OVMFS_OK && mapped_count == 0);
    CHECK(ovmfs_disk_map(disk, 1, ovmfs_disk_capacity(disk), mapped, 4, &mapped_count, &error) == OVMFS_ERR_INVALID_INPUT);
    ovmfs_error_free(error);
    error = NULL;

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
    CHECK(check_caller_supplied_device(argv[1]) == 0);
    printf("OK data-byte-sum=%llu\n", sum);
    return 0;
}
