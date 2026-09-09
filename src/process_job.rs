//! Windows process-tree lifetime guard. Not an execution sandbox.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::System::JobObjects::*;

pub struct ProcessJob(OwnedHandle);

impl ProcessJob {
    pub fn attach(child: &tokio::process::Child) -> std::io::Result<Self> {
        // SAFETY: null attributes/name create an unnamed non-inheritable job.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: CreateJobObjectW returned a unique valid handle we now own.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: valid handle and pointer to the indicated initialized struct.
        if unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let process = child
            .raw_handle()
            .ok_or_else(|| std::io::Error::other("child has no process handle"))?;
        // SAFETY: both handles remain live for the duration of this call.
        if unsafe { AssignProcessToJobObject(job.0.as_raw_handle(), process) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(job)
    }

    pub fn terminate(&self) -> std::io::Result<()> {
        // SAFETY: this guard owns a valid job handle for this task's processes.
        if unsafe { TerminateJobObject(self.0.as_raw_handle(), 1) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}
