#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/input.h>
#include <linux/uinput.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <time.h>
#include <unistd.h>

typedef struct {
    uint8_t slot;
    uint8_t connected;
    uint16_t buttons;
    uint8_t left_trigger;
    uint8_t right_trigger;
    int16_t left_x;
    int16_t left_y;
    int16_t right_x;
    int16_t right_y;
} RemotersGamepadState;

enum {
    REMOTERS_GP_DPAD_UP = 0x0001,
    REMOTERS_GP_DPAD_DOWN = 0x0002,
    REMOTERS_GP_DPAD_LEFT = 0x0004,
    REMOTERS_GP_DPAD_RIGHT = 0x0008,
    REMOTERS_GP_START = 0x0010,
    REMOTERS_GP_BACK = 0x0020,
    REMOTERS_GP_LEFT_THUMB = 0x0040,
    REMOTERS_GP_RIGHT_THUMB = 0x0080,
    REMOTERS_GP_LEFT_SHOULDER = 0x0100,
    REMOTERS_GP_RIGHT_SHOULDER = 0x0200,
    REMOTERS_GP_GUIDE = 0x0400,
    REMOTERS_GP_A = 0x1000,
    REMOTERS_GP_B = 0x2000,
    REMOTERS_GP_X = 0x4000,
    REMOTERS_GP_Y = 0x8000,
};

#define BIT_TEST(arr, bit) (((arr)[(bit) / 8] >> ((bit) % 8)) & 1)

typedef struct {
    int fd;
    char path[280];
    struct input_absinfo abs_x;
    struct input_absinfo abs_y;
    struct input_absinfo abs_rx;
    struct input_absinfo abs_ry;
    struct input_absinfo abs_z;
    struct input_absinfo abs_rz;
    bool has_abs_x;
    bool has_abs_y;
    bool has_abs_rx;
    bool has_abs_ry;
    bool has_abs_z;
    bool has_abs_rz;
    int32_t hat_x;
    int32_t hat_y;
    uint16_t dpad_btn_mask;
    RemotersGamepadState state;
} LinuxEvdevSlot;

typedef struct {
    LinuxEvdevSlot slots[4];
    uint64_t last_scan_ms;
} LinuxGamepadReader;

static uint64_t monotonic_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint64_t)ts.tv_sec * 1000ULL + (uint64_t)(ts.tv_nsec / 1000000L);
}

static int16_t scale_axis_i16(int32_t val, const struct input_absinfo *info, bool invert) {
    int32_t lo = info->minimum;
    int32_t hi = info->maximum;
    if (hi <= lo) {
        lo = -32768;
        hi = 32767;
    }
    if (val < lo) {
        val = lo;
    } else if (val > hi) {
        val = hi;
    }
    double norm = ((double)(val - lo) / (double)(hi - lo)) * 2.0 - 1.0;
    if (invert) {
        norm = -norm;
    }
    if (norm >= 1.0) {
        return 32767;
    }
    if (norm <= -1.0) {
        return -32768;
    }
    long scaled = (long)(norm * 32767.0 + (norm >= 0.0 ? 0.5 : -0.5));
    if (scaled > 32767) {
        return 32767;
    }
    if (scaled < -32768) {
        return -32768;
    }
    return (int16_t)scaled;
}

static uint8_t scale_trigger_u8(int32_t val, const struct input_absinfo *info) {
    int32_t lo = info->minimum;
    int32_t hi = info->maximum;
    if (hi <= lo) {
        lo = 0;
        hi = 255;
    }
    if (val < lo) {
        val = lo;
    } else if (val > hi) {
        val = hi;
    }
    double norm = (double)(val - lo) / (double)(hi - lo);
    long scaled = (long)(norm * 255.0 + 0.5);
    if (scaled < 0) {
        return 0;
    }
    if (scaled > 255) {
        return 255;
    }
    return (uint8_t)scaled;
}

static bool is_gamepad_fd(int fd) {
    uint8_t ev_bits[(EV_MAX / 8) + 1];
    uint8_t key_bits[(KEY_MAX / 8) + 1];
    uint8_t abs_bits[(ABS_MAX / 8) + 1];
    memset(ev_bits, 0, sizeof(ev_bits));
    memset(key_bits, 0, sizeof(key_bits));
    memset(abs_bits, 0, sizeof(abs_bits));

    if (ioctl(fd, EVIOCGBIT(0, sizeof(ev_bits)), ev_bits) < 0) {
        return false;
    }
    if (!BIT_TEST(ev_bits, EV_KEY) || !BIT_TEST(ev_bits, EV_ABS)) {
        return false;
    }
    if (ioctl(fd, EVIOCGBIT(EV_KEY, sizeof(key_bits)), key_bits) < 0) {
        return false;
    }
    if (ioctl(fd, EVIOCGBIT(EV_ABS, sizeof(abs_bits)), abs_bits) < 0) {
        return false;
    }

    bool has_gamepad_btn = BIT_TEST(key_bits, BTN_SOUTH) || BIT_TEST(key_bits, BTN_GAMEPAD) ||
                           BIT_TEST(key_bits, BTN_TRIGGER);
    bool has_stick_abs = BIT_TEST(abs_bits, ABS_X) && BIT_TEST(abs_bits, ABS_Y);
    return has_gamepad_btn && has_stick_abs;
}

static void init_slot_from_fd(LinuxEvdevSlot *slot, int fd, const char *path, uint8_t index) {
    memset(slot, 0, sizeof(*slot));
    slot->fd = fd;
    snprintf(slot->path, sizeof(slot->path), "%s", path);
    slot->state.slot = index;
    slot->state.connected = 1;

    slot->has_abs_x = ioctl(fd, EVIOCGABS(ABS_X), &slot->abs_x) == 0;
    slot->has_abs_y = ioctl(fd, EVIOCGABS(ABS_Y), &slot->abs_y) == 0;
    slot->has_abs_rx = ioctl(fd, EVIOCGABS(ABS_RX), &slot->abs_rx) == 0;
    slot->has_abs_ry = ioctl(fd, EVIOCGABS(ABS_RY), &slot->abs_ry) == 0;
    slot->has_abs_z = ioctl(fd, EVIOCGABS(ABS_Z), &slot->abs_z) == 0;
    slot->has_abs_rz = ioctl(fd, EVIOCGABS(ABS_RZ), &slot->abs_rz) == 0;
}

static void close_slot(LinuxEvdevSlot *slot, uint8_t index) {
    if (slot->fd >= 0) {
        close(slot->fd);
    }
    memset(slot, 0, sizeof(*slot));
    slot->fd = -1;
    slot->state.slot = index;
}

static void scan_evdev(LinuxGamepadReader *reader) {
    DIR *dir = opendir("/dev/input");
    if (!dir) {
        return;
    }
    struct dirent *entry;
    while ((entry = readdir(dir)) != NULL) {
        if (strncmp(entry->d_name, "event", 5) != 0) {
            continue;
        }
        char path[280];
        snprintf(path, sizeof(path), "/dev/input/%s", entry->d_name);

        bool already_open = false;
        int free_slot = -1;
        for (int i = 0; i < 4; ++i) {
            if (reader->slots[i].fd >= 0 && strcmp(reader->slots[i].path, path) == 0) {
                already_open = true;
                break;
            }
            if (reader->slots[i].fd < 0 && free_slot < 0) {
                free_slot = i;
            }
        }
        if (already_open || free_slot < 0) {
            continue;
        }

        int fd = open(path, O_RDONLY | O_NONBLOCK);
        if (fd < 0) {
            continue;
        }
        if (!is_gamepad_fd(fd)) {
            close(fd);
            continue;
        }
        init_slot_from_fd(&reader->slots[free_slot], fd, path, (uint8_t)free_slot);
    }
    closedir(dir);
}

static void update_dpad_buttons(LinuxEvdevSlot *slot) {
    uint16_t hat_mask = 0;
    if (slot->hat_y < 0) {
        hat_mask |= REMOTERS_GP_DPAD_UP;
    } else if (slot->hat_y > 0) {
        hat_mask |= REMOTERS_GP_DPAD_DOWN;
    }
    if (slot->hat_x < 0) {
        hat_mask |= REMOTERS_GP_DPAD_LEFT;
    } else if (slot->hat_x > 0) {
        hat_mask |= REMOTERS_GP_DPAD_RIGHT;
    }
    const uint16_t dpad_bits = REMOTERS_GP_DPAD_UP | REMOTERS_GP_DPAD_DOWN |
                               REMOTERS_GP_DPAD_LEFT | REMOTERS_GP_DPAD_RIGHT;
    slot->state.buttons = (uint16_t)((slot->state.buttons & ~dpad_bits) | hat_mask | slot->dpad_btn_mask);
}

static void apply_evdev_event(LinuxEvdevSlot *slot, const struct input_event *ev) {
    if (ev->type == EV_KEY) {
        bool down = ev->value != 0;
        uint16_t bit = 0;
        switch (ev->code) {
        case BTN_SOUTH:
            bit = REMOTERS_GP_A;
            break;
        case BTN_EAST:
            bit = REMOTERS_GP_B;
            break;
        case BTN_WEST:
            bit = REMOTERS_GP_X;
            break;
        case BTN_NORTH:
            bit = REMOTERS_GP_Y;
            break;
        case BTN_TL:
            bit = REMOTERS_GP_LEFT_SHOULDER;
            break;
        case BTN_TR:
            bit = REMOTERS_GP_RIGHT_SHOULDER;
            break;
        case BTN_SELECT:
            bit = REMOTERS_GP_BACK;
            break;
        case BTN_START:
            bit = REMOTERS_GP_START;
            break;
        case BTN_MODE:
            bit = REMOTERS_GP_GUIDE;
            break;
        case BTN_THUMBL:
            bit = REMOTERS_GP_LEFT_THUMB;
            break;
        case BTN_THUMBR:
            bit = REMOTERS_GP_RIGHT_THUMB;
            break;
        case BTN_TL2:
            if (!slot->has_abs_z) {
                slot->state.left_trigger = down ? 255 : 0;
            }
            return;
        case BTN_TR2:
            if (!slot->has_abs_rz) {
                slot->state.right_trigger = down ? 255 : 0;
            }
            return;
        case BTN_DPAD_UP:
            if (down) slot->dpad_btn_mask |= REMOTERS_GP_DPAD_UP;
            else slot->dpad_btn_mask &= ~REMOTERS_GP_DPAD_UP;
            update_dpad_buttons(slot);
            return;
        case BTN_DPAD_DOWN:
            if (down) slot->dpad_btn_mask |= REMOTERS_GP_DPAD_DOWN;
            else slot->dpad_btn_mask &= ~REMOTERS_GP_DPAD_DOWN;
            update_dpad_buttons(slot);
            return;
        case BTN_DPAD_LEFT:
            if (down) slot->dpad_btn_mask |= REMOTERS_GP_DPAD_LEFT;
            else slot->dpad_btn_mask &= ~REMOTERS_GP_DPAD_LEFT;
            update_dpad_buttons(slot);
            return;
        case BTN_DPAD_RIGHT:
            if (down) slot->dpad_btn_mask |= REMOTERS_GP_DPAD_RIGHT;
            else slot->dpad_btn_mask &= ~REMOTERS_GP_DPAD_RIGHT;
            update_dpad_buttons(slot);
            return;
        default:
            return;
        }
        if (down) {
            slot->state.buttons |= bit;
        } else {
            slot->state.buttons &= (uint16_t)~bit;
        }
    } else if (ev->type == EV_ABS) {
        switch (ev->code) {
        case ABS_X:
            if (slot->has_abs_x) {
                slot->state.left_x = scale_axis_i16(ev->value, &slot->abs_x, false);
            }
            break;
        case ABS_Y:
            if (slot->has_abs_y) {
                slot->state.left_y = scale_axis_i16(ev->value, &slot->abs_y, true);
            }
            break;
        case ABS_RX:
            if (slot->has_abs_rx) {
                slot->state.right_x = scale_axis_i16(ev->value, &slot->abs_rx, false);
            }
            break;
        case ABS_RY:
            if (slot->has_abs_ry) {
                slot->state.right_y = scale_axis_i16(ev->value, &slot->abs_ry, true);
            }
            break;
        case ABS_Z:
            if (slot->has_abs_z) {
                slot->state.left_trigger = scale_trigger_u8(ev->value, &slot->abs_z);
            }
            break;
        case ABS_RZ:
            if (slot->has_abs_rz) {
                slot->state.right_trigger = scale_trigger_u8(ev->value, &slot->abs_rz);
            }
            break;
        case ABS_HAT0X:
            slot->hat_x = ev->value;
            update_dpad_buttons(slot);
            break;
        case ABS_HAT0Y:
            slot->hat_y = ev->value;
            update_dpad_buttons(slot);
            break;
        default:
            break;
        }
    }
}

void *remoters_linux_gamepad_reader_create(void) {
    LinuxGamepadReader *reader = (LinuxGamepadReader *)calloc(1, sizeof(LinuxGamepadReader));
    if (!reader) {
        return NULL;
    }
    for (int i = 0; i < 4; ++i) {
        reader->slots[i].fd = -1;
        reader->slots[i].state.slot = (uint8_t)i;
    }
    scan_evdev(reader);
    reader->last_scan_ms = monotonic_ms();
    return reader;
}

void remoters_linux_gamepad_reader_poll(void *ptr, RemotersGamepadState out_states[4]) {
    if (!ptr || !out_states) {
        return;
    }
    LinuxGamepadReader *reader = (LinuxGamepadReader *)ptr;
    uint64_t now = monotonic_ms();
    if (now - reader->last_scan_ms >= 1000) {
        scan_evdev(reader);
        reader->last_scan_ms = now;
    }

    for (int i = 0; i < 4; ++i) {
        LinuxEvdevSlot *slot = &reader->slots[i];
        if (slot->fd >= 0) {
            struct input_event ev;
            for (;;) {
                ssize_t n = read(slot->fd, &ev, sizeof(ev));
                if (n == (ssize_t)sizeof(ev)) {
                    apply_evdev_event(slot, &ev);
                } else if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) {
                    break;
                } else {
                    close_slot(slot, (uint8_t)i);
                    break;
                }
            }
        }
        out_states[i] = slot->state;
        out_states[i].slot = (uint8_t)i;
    }
}

void remoters_linux_gamepad_reader_destroy(void *ptr) {
    if (!ptr) {
        return;
    }
    LinuxGamepadReader *reader = (LinuxGamepadReader *)ptr;
    for (int i = 0; i < 4; ++i) {
        close_slot(&reader->slots[i], (uint8_t)i);
    }
    free(reader);
}

typedef struct {
    int fds[4];
    bool unavailable;
    char last_error[256];
} LinuxUinputHost;

static int open_uinput_xbox360(char *err_buf, size_t err_cap) {
    int fd = open("/dev/uinput", O_WRONLY | O_NONBLOCK);
    if (fd < 0) {
        fd = open("/dev/input/uinput", O_WRONLY | O_NONBLOCK);
    }
    if (fd < 0) {
        snprintf(err_buf, err_cap, "Failed to open /dev/uinput: %s", strerror(errno));
        return -1;
    }

    if (ioctl(fd, UI_SET_EVBIT, EV_KEY) < 0 ||
        ioctl(fd, UI_SET_EVBIT, EV_ABS) < 0 ||
        ioctl(fd, UI_SET_EVBIT, EV_SYN) < 0) {
        snprintf(err_buf, err_cap, "uinput UI_SET_EVBIT failed: %s", strerror(errno));
        close(fd);
        return -1;
    }

    const int keys[] = {
        BTN_SOUTH, BTN_EAST, BTN_NORTH, BTN_WEST,
        BTN_TL, BTN_TR, BTN_SELECT, BTN_START,
        BTN_MODE, BTN_THUMBL, BTN_THUMBR,
    };
    for (size_t i = 0; i < sizeof(keys) / sizeof(keys[0]); ++i) {
        if (ioctl(fd, UI_SET_KEYBIT, keys[i]) < 0) {
            snprintf(err_buf, err_cap, "uinput UI_SET_KEYBIT failed: %s", strerror(errno));
            close(fd);
            return -1;
        }
    }

    const int axes[] = {
        ABS_X, ABS_Y, ABS_RX, ABS_RY, ABS_Z, ABS_RZ, ABS_HAT0X, ABS_HAT0Y,
    };
    for (size_t i = 0; i < sizeof(axes) / sizeof(axes[0]); ++i) {
        if (ioctl(fd, UI_SET_ABSBIT, axes[i]) < 0) {
            snprintf(err_buf, err_cap, "uinput UI_SET_ABSBIT failed: %s", strerror(errno));
            close(fd);
            return -1;
        }
    }

    struct uinput_user_dev udev;
    memset(&udev, 0, sizeof(udev));
    snprintf(udev.name, UINPUT_MAX_NAME_SIZE, "Microsoft X-Box 360 pad");
    udev.id.bustype = BUS_USB;
    udev.id.vendor = 0x045e;
    udev.id.product = 0x028e;
    udev.id.version = 0x0110;

    udev.absmin[ABS_X] = -32768;
    udev.absmax[ABS_X] = 32767;
    udev.absmin[ABS_Y] = -32768;
    udev.absmax[ABS_Y] = 32767;
    udev.absmin[ABS_RX] = -32768;
    udev.absmax[ABS_RX] = 32767;
    udev.absmin[ABS_RY] = -32768;
    udev.absmax[ABS_RY] = 32767;
    udev.absmin[ABS_Z] = 0;
    udev.absmax[ABS_Z] = 255;
    udev.absmin[ABS_RZ] = 0;
    udev.absmax[ABS_RZ] = 255;
    udev.absmin[ABS_HAT0X] = -1;
    udev.absmax[ABS_HAT0X] = 1;
    udev.absmin[ABS_HAT0Y] = -1;
    udev.absmax[ABS_HAT0Y] = 1;

    if (write(fd, &udev, sizeof(udev)) != (ssize_t)sizeof(udev)) {
        snprintf(err_buf, err_cap, "uinput write(uinput_user_dev) failed: %s", strerror(errno));
        close(fd);
        return -1;
    }
    if (ioctl(fd, UI_DEV_CREATE) < 0) {
        snprintf(err_buf, err_cap, "uinput UI_DEV_CREATE failed: %s", strerror(errno));
        close(fd);
        return -1;
    }
    return fd;
}

static void emit_uinput_event(int fd, uint16_t type, uint16_t code, int32_t value) {
    struct input_event ev;
    memset(&ev, 0, sizeof(ev));
    ev.type = type;
    ev.code = code;
    ev.value = value;
    ssize_t unused = write(fd, &ev, sizeof(ev));
    (void)unused;
}

static int32_t invert_i16_axis(int16_t v) {
    if (v == INT16_MIN) {
        return 32767;
    }
    return -(int32_t)v;
}

void *remoters_linux_uinput_create(void) {
    LinuxUinputHost *host = (LinuxUinputHost *)calloc(1, sizeof(LinuxUinputHost));
    if (!host) {
        return NULL;
    }
    for (int i = 0; i < 4; ++i) {
        host->fds[i] = -1;
    }
    return host;
}

bool remoters_linux_uinput_apply(void *ptr, const RemotersGamepadState *state) {
    if (!ptr || !state) {
        return false;
    }
    LinuxUinputHost *host = (LinuxUinputHost *)ptr;
    if (state->slot >= 4) {
        return true;
    }
    uint8_t slot = state->slot;
    if (!state->connected) {
        if (host->fds[slot] >= 0) {
            ioctl(host->fds[slot], UI_DEV_DESTROY);
            close(host->fds[slot]);
            host->fds[slot] = -1;
        }
        return true;
    }
    if (host->unavailable) {
        return false;
    }
    if (host->fds[slot] < 0) {
        int fd = open_uinput_xbox360(host->last_error, sizeof(host->last_error));
        if (fd < 0) {
            host->unavailable = true;
            return false;
        }
        host->fds[slot] = fd;
    }

    int fd = host->fds[slot];
    uint16_t b = state->buttons;
    emit_uinput_event(fd, EV_KEY, BTN_SOUTH, (b & REMOTERS_GP_A) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_EAST, (b & REMOTERS_GP_B) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_WEST, (b & REMOTERS_GP_X) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_NORTH, (b & REMOTERS_GP_Y) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_TL, (b & REMOTERS_GP_LEFT_SHOULDER) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_TR, (b & REMOTERS_GP_RIGHT_SHOULDER) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_SELECT, (b & REMOTERS_GP_BACK) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_START, (b & REMOTERS_GP_START) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_MODE, (b & REMOTERS_GP_GUIDE) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_THUMBL, (b & REMOTERS_GP_LEFT_THUMB) != 0);
    emit_uinput_event(fd, EV_KEY, BTN_THUMBR, (b & REMOTERS_GP_RIGHT_THUMB) != 0);

    emit_uinput_event(fd, EV_ABS, ABS_X, state->left_x);
    emit_uinput_event(fd, EV_ABS, ABS_Y, invert_i16_axis(state->left_y));
    emit_uinput_event(fd, EV_ABS, ABS_RX, state->right_x);
    emit_uinput_event(fd, EV_ABS, ABS_RY, invert_i16_axis(state->right_y));
    emit_uinput_event(fd, EV_ABS, ABS_Z, state->left_trigger);
    emit_uinput_event(fd, EV_ABS, ABS_RZ, state->right_trigger);

    int32_t hat_x = ((b & REMOTERS_GP_DPAD_RIGHT) != 0) - ((b & REMOTERS_GP_DPAD_LEFT) != 0);
    int32_t hat_y = ((b & REMOTERS_GP_DPAD_DOWN) != 0) - ((b & REMOTERS_GP_DPAD_UP) != 0);
    emit_uinput_event(fd, EV_ABS, ABS_HAT0X, hat_x);
    emit_uinput_event(fd, EV_ABS, ABS_HAT0Y, hat_y);

    emit_uinput_event(fd, EV_SYN, SYN_REPORT, 0);
    return true;
}

void remoters_linux_uinput_reset(void *ptr) {
    if (!ptr) {
        return;
    }
    LinuxUinputHost *host = (LinuxUinputHost *)ptr;
    for (int i = 0; i < 4; ++i) {
        if (host->fds[i] >= 0) {
            ioctl(host->fds[i], UI_DEV_DESTROY);
            close(host->fds[i]);
            host->fds[i] = -1;
        }
    }
}

void remoters_linux_uinput_destroy(void *ptr) {
    if (!ptr) {
        return;
    }
    remoters_linux_uinput_reset(ptr);
    free(ptr);
}

size_t remoters_linux_uinput_last_error(void *ptr, char *buf, size_t cap) {
    if (!ptr || !buf || cap == 0) {
        return 0;
    }
    LinuxUinputHost *host = (LinuxUinputHost *)ptr;
    size_t n = strlen(host->last_error);
    if (n >= cap) {
        n = cap - 1;
    }
    memcpy(buf, host->last_error, n);
    buf[n] = '\0';
    return n;
}
