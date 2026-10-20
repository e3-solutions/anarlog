const PROC_PIDTBSDINFO: i32 = 3;
const PROC_PIDT_SHORTBSDINFO: i32 = 13;
// <sys/proc.h>: an exited process awaiting collection by its parent.
const SZOMB: u32 = 5;
const MAXCOMLEN: usize = 16;

#[repr(C)]
struct ProcBsdShortInfo {
    pbsi_pid: u32,
    pbsi_ppid: u32,
    pbsi_pgid: u32,
    pbsi_status: u32,
    pbsi_comm: [u8; MAXCOMLEN],
    pbsi_flags: u32,
    pbsi_uid: u32,
    pbsi_gid: u32,
    pbsi_ruid: u32,
    pbsi_rgid: u32,
    pbsi_svuid: u32,
    pbsi_svgid: u32,
    pbsi_rfu: u32,
}

// Public Darwin proc_bsdinfo layout from <sys/proc_info.h>.
#[repr(C)]
struct ProcBsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: u32,
    pbi_gid: u32,
    pbi_ruid: u32,
    pbi_rgid: u32,
    pbi_svuid: u32,
    pbi_svgid: u32,
    rfu_1: u32,
    pbi_comm: [u8; MAXCOMLEN],
    pbi_name: [u8; 2 * MAXCOMLEN],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

unsafe extern "C" {
    fn proc_pidinfo(
        pid: i32,
        flavor: i32,
        arg: u64,
        buffer: *mut std::ffi::c_void,
        buffersize: i32,
    ) -> i32;
}

fn bsd_info_for_pid(pid: i32) -> Option<ProcBsdInfo> {
    if pid <= 0 {
        return None;
    }
    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::uninit();
    let size = std::mem::size_of::<ProcBsdInfo>() as i32;
    let ret = unsafe {
        proc_pidinfo(
            pid,
            PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr() as *mut std::ffi::c_void,
            size,
        )
    };
    if ret != size {
        return None;
    }
    let info = unsafe { info.assume_init() };
    (info.pbi_pid == pid as u32).then_some(info)
}

/// Kernel process birth stamp, independent of AppKit application caches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    pid: u32,
    start_seconds: u64,
    start_microseconds: u64,
}

pub fn process_identity_for_pid(pid: i32) -> Option<ProcessIdentity> {
    let info = bsd_info_for_pid(pid)?;
    if info.pbi_status == SZOMB || info.pbi_start_tvsec == 0 || info.pbi_start_tvusec >= 1_000_000 {
        return None;
    }
    Some(ProcessIdentity {
        pid: info.pbi_pid,
        start_seconds: info.pbi_start_tvsec,
        start_microseconds: info.pbi_start_tvusec,
    })
}

pub fn parent_pid_for_pid(pid: u32) -> Option<u32> {
    let mut info = std::mem::MaybeUninit::<ProcBsdShortInfo>::uninit();
    let ret = unsafe {
        proc_pidinfo(
            pid as i32,
            PROC_PIDT_SHORTBSDINFO,
            0,
            info.as_mut_ptr() as *mut std::ffi::c_void,
            std::mem::size_of::<ProcBsdShortInfo>() as i32,
        )
    };

    if ret >= std::mem::size_of::<ProcBsdShortInfo>() as i32 {
        Some(unsafe { info.assume_init() }.pbsi_ppid)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_identity_tracks_a_live_process_and_rejects_its_exit() {
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let first = process_identity_for_pid(pid);
        let second = process_identity_for_pid(pid);
        // Reap the child before assertions, including when a read failed.
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(first.is_some());
        assert_eq!(first, second);
        assert_ne!(first, process_identity_for_pid(std::process::id() as i32));
        assert_eq!(process_identity_for_pid(pid), None);
        assert_eq!(process_identity_for_pid(0), None);
        assert_eq!(process_identity_for_pid(-1), None);
    }

    #[test]
    fn test_parent_pid_for_pid() {
        let pid = std::process::id();
        let parent_pid = parent_pid_for_pid(pid);
        assert!(parent_pid.is_some());
    }
}
