const std = @import("std");
const linux = std.os.linux;

const Mode = enum {
    noop,
    standby,
    @"suspend",
    hibernate,
};

const HibernateMode = enum {
    platform,
    reboot,
    suspend_hybrid,
};

const sys_power = "/sys/power";

fn fatal(msg: []const u8) noreturn {
    _ = linux.write(2, msg.ptr, msg.len);
    linux.exit(1);
}

fn prognam(arg0: []const u8) []const u8 {
    return if (std.mem.lastIndexOfScalar(u8, arg0, '/')) |i|
        arg0[i + 1 ..]
    else
        arg0;
}

fn stateSupports(needle: []const u8) bool {
    const fd = linux.open(sys_power ++ "/state", .{}, 0);
    if (fd >= ~@as(usize, 4095)) return false;
    defer _ = linux.close(@intCast(fd));

    var buf: [256]u8 = undefined;
    const n = linux.read(@intCast(fd), &buf, buf.len);
    if (n >= ~@as(usize, 4095)) return false;
    return std.mem.indexOf(u8, buf[0..n], needle) != null;
}

fn writeSysfs(path: [*:0]const u8, value: []const u8) void {
    const fd = linux.open(path, .{ .ACCMODE = .WRONLY }, 0);
    if (fd >= ~@as(usize, 4095)) return;
    defer _ = linux.close(@intCast(fd));
    _ = linux.write(@intCast(fd), value.ptr, value.len);
}

fn runHooks(dirpath: [*:0]const u8) void {
    const dirfd = linux.open(dirpath, .{ .DIRECTORY = true }, 0);
    if (dirfd >= ~@as(usize, 4095)) return;
    defer _ = linux.close(@intCast(dirfd));

    var buf: [4096]u8 align(@alignOf(linux.dirent64)) = undefined;
    while (true) {
        const n = linux.getdents64(@intCast(dirfd), &buf, buf.len);
        if (n >= ~@as(usize, 4095) or n == 0) break;
        var pos: usize = 0;
        while (pos < n) {
            const d: *linux.dirent64 = @ptrCast(@alignCast(&buf[pos]));
            pos += d.reclen;
            const name = std.mem.sliceTo(@as([*:0]u8, @ptrCast(&d.name)), 0);
            if (name[0] == '.') continue;
            if (d.type != linux.DT.REG and d.type != linux.DT.UNKNOWN) continue;

            var path_buf: [512]u8 = undefined;
            const path = std.fmt.bufPrintZ(&path_buf, "{s}/{s}", .{ dirpath, name }) catch continue;

            if (linux.access(path, linux.X_OK) != 0) continue;

            const pid = linux.fork();
            if (pid == 0) {
                _ = linux.execve(path, @ptrCast(&[_:null]?[*:0]u8{ @ptrCast(path.ptr), null }), @ptrCast(&[_:null]?[*:0]u8{null}));
                linux.exit(1);
            }
            if (pid != ~@as(usize, 0)) {
                var status: u32 = 0;
                _ = linux.wait4(@intCast(pid), &status, 0, null);
            }
        }
    }
}

pub fn main(init: std.process.Init.Minimal) void {
    var args_iter = std.process.Args.iterate(init.args);

    const arg0 = args_iter.next() orelse "zzz";
    const name = prognam(arg0);

    var mode: Mode = if (std.mem.eql(u8, name, "ZZZ")) .hibernate else .@"suspend";
    var hiber_mode: HibernateMode = .platform;

    while (args_iter.next()) |arg| {
        if (arg.len < 2 or arg[0] != '-') fatal("bad arg\n");
        for (arg[1..]) |c| {
            switch (c) {
                'n' => mode = .noop,
                'S' => mode = .standby,
                'z' => mode = .@"suspend",
                'Z' => mode = .hibernate,
                'R' => { mode = .hibernate; hiber_mode = .reboot; },
                'H' => { mode = .hibernate; hiber_mode = .suspend_hybrid; },
                else => fatal("bad arg\n"),
            }
        }
    }

    switch (mode) {
        .@"suspend" => if (!stateSupports("mem")) fatal("suspend not supported\n"),
        .hibernate => if (!stateSupports("disk")) fatal("hibernate not supported\n"),
        else => {},
    }

    // Use open() rather than access() — access() checks real UID, so it
    // rejects setuid-root binaries even though the actual writes would
    // succeed. open(WRONLY) tests the effective UID instead.
    const checkfd = linux.open(sys_power ++ "/state", .{ .ACCMODE = .WRONLY }, 0);
    if (checkfd >= ~@as(usize, 4095))
        fatal("sleep permission denied\n");
    _ = linux.close(@intCast(checkfd));

    // flock /sys/power
    const lockfd = linux.open(sys_power, .{}, 0);
    if (lockfd >= ~@as(usize, 4095)) fatal("cannot open /sys/power\n");
    defer _ = linux.close(@intCast(lockfd));

    if (linux.flock(@intCast(lockfd), 2 | 4) != 0) // LOCK_EX | LOCK_NB
        fatal("another instance of zzz is running\n");
    defer _ = linux.flock(@intCast(lockfd), 8); // LOCK_UN

    _ = linux.write(1, "Zzzz... ", 8);

    runHooks("/etc/zzz.d/suspend");

    switch (mode) {
        .standby => writeSysfs(sys_power ++ "/state", "freeze"),
        .@"suspend" => writeSysfs(sys_power ++ "/state", "mem"),
        .hibernate => {
            const disk_mode: []const u8 = switch (hiber_mode) {
                .platform => "platform",
                .reboot => "reboot",
                .suspend_hybrid => "suspend",
            };
            writeSysfs(sys_power ++ "/disk", disk_mode);
            writeSysfs(sys_power ++ "/state", "disk");
        },
        .noop => {
            var ts: linux.timespec = .{ .sec = 5, .nsec = 0 };
            _ = linux.nanosleep(&ts, null);
        },
    }

    runHooks("/etc/zzz.d/resume");

    _ = linux.write(1, "yawn.\n", 6);
}
