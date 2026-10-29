pub(crate) trait Updater {
    type Error;

    fn enabled(&self) -> Result<bool, Self::Error>;
    fn meeting_active(&self) -> bool;
    async fn check(&self) -> Result<Option<String>, Self::Error>;
    async fn download(&self, version: &str) -> Result<(), Self::Error>;
    async fn install(&self, version: &str) -> Result<(), Self::Error>;
}

pub(crate) async fn run<U: Updater>(updater: &U) -> Result<(), U::Error> {
    if !updater.enabled()? || updater.meeting_active() {
        return Ok(());
    }

    let Some(version) = updater.check().await? else {
        return Ok(());
    };

    if !updater.enabled()? || updater.meeting_active() {
        return Ok(());
    }

    updater.download(&version).await?;

    // Downloads can span the start of a meeting or an opt-out in Settings.
    if !updater.enabled()? || updater.meeting_active() {
        return Ok(());
    }

    updater.install(&version).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::Cell,
        future::Future,
        task::{Context, Poll, Waker},
    };

    #[derive(Default)]
    struct Device {
        disabled: Cell<bool>,
        meeting: Cell<bool>,
        available: Cell<bool>,
        downloaded: Cell<bool>,
        installed: Cell<bool>,
        start_meeting_during_download: Cell<bool>,
        disable_during_download: Cell<bool>,
        fail_install: Cell<bool>,
    }

    impl Updater for Device {
        type Error = &'static str;

        fn enabled(&self) -> Result<bool, Self::Error> {
            Ok(!self.disabled.get())
        }
        fn meeting_active(&self) -> bool {
            self.meeting.get()
        }
        async fn check(&self) -> Result<Option<String>, Self::Error> {
            Ok(self.available.get().then(|| "0.1.28".to_string()))
        }
        async fn download(&self, _: &str) -> Result<(), Self::Error> {
            self.downloaded.set(true);
            if self.start_meeting_during_download.replace(false) {
                self.meeting.set(true);
            }
            if self.disable_during_download.replace(false) {
                self.disabled.set(true);
            }
            Ok(())
        }
        async fn install(&self, _: &str) -> Result<(), Self::Error> {
            if self.fail_install.replace(false) {
                return Err("temporary install failure");
            }
            self.installed.set(true);
            Ok(())
        }
    }

    fn tick(device: &Device) -> Result<(), &'static str> {
        let mut future = std::pin::pin!(run(device));
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(result) => result,
            Poll::Pending => panic!("test device operations must complete immediately"),
        }
    }

    #[test]
    fn installs_a_release_published_after_launch_without_reopening_or_clicking() {
        let device = Device::default();
        tick(&device).unwrap();
        assert!(!device.installed.get());

        device.available.set(true);
        device.start_meeting_during_download.set(true);
        tick(&device).unwrap();
        assert!(device.downloaded.get());
        assert!(!device.installed.get(), "must not restart during recording");
        tick(&device).unwrap();
        assert!(!device.installed.get());

        device.meeting.set(false);
        device.fail_install.set(true);
        assert!(tick(&device).is_err());
        assert!(!device.installed.get());
        tick(&device).unwrap();
        assert!(
            device.installed.get(),
            "must retry without reopening the app"
        );
    }

    #[test]
    fn honors_an_opt_out_while_an_update_is_downloading() {
        let device = Device::default();
        device.available.set(true);
        device.disable_during_download.set(true);
        tick(&device).unwrap();
        tick(&device).unwrap();
        assert!(!device.installed.get());
        device.disabled.set(false);
        tick(&device).unwrap();
        assert!(device.installed.get());
    }
}
