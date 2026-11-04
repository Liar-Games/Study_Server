use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[test]
fn disabling_guests_without_an_auth_provider_refuses_to_start() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_study_server"))
        .env_remove("REDIS_URL")
        .env("ALLOW_GUESTS", "false")
        .env("PORT", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            return;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("server accepted guest-only mode despite ALLOW_GUESTS=false");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
