//! A graphics chip on trial: programs draw on it until one of them ends
//! before it has shown anything, and on the CPU from then on.
//!
//! Some chips are not known to run a program that draws on them inside a
//! display server: Intel's up to the third generation (`old_intel`), whose
//! driver is the least exercised Mesa has. On a GMA 3150 no app stayed
//! open the first time they were given the GPU, and they were kept off it
//! from then on, though what ended them (shaders too long for the chip)
//! has since been put right and the desktop itself is drawn by that chip.
//! Keeping them off costs every frame of every app; giving them the chip
//! unasked risks a machine on which nothing opens.
//!
//! So each program says it is trying: a file named for its process, made
//! before it starts the GPU and removed once it has handed a few frames
//! over, or when it closes in order. A program that finds such a file
//! whose process is gone knows the trial ended badly, says so in a second
//! file, and draws on the CPU, as every program after it does. One app
//! fails to open, once; the files are in the temporary folder, so the
//! chip is tried again the next time the machine is started.

use std::path::{Path, PathBuf};

/// Frames handed over before a trial is called passed. What ended apps on
/// the chip this was written for ended them before the first.
const ENOUGH: u32 = 3;

const TRYING: &str = "shine-gpu-trial-";
const FAILED: &str = "shine-gpu-failed";

/// Where the files are kept: the temporary folder, which every program of
/// every account can write to and which is empty when the machine starts.
pub(crate) fn folder() -> PathBuf {
    std::env::temp_dir()
}

fn alive(process: u32) -> bool {
    Path::new("/proc").join(process.to_string()).exists()
}

/// Whether a program's trial of the chip ended badly, by the files in
/// `folder`: said already, or found now, in a file left by a process that
/// `alive` says is gone.
pub(crate) fn failed(folder: &Path) -> bool {
    failed_by(folder, alive)
}

fn failed_by(folder: &Path, alive: impl Fn(u32) -> bool) -> bool {
    if folder.join(FAILED).exists() {
        return true;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(process) = name
            .to_str()
            .and_then(|name| name.strip_prefix(TRYING))
            .and_then(|process| process.parse::<u32>().ok())
        else {
            continue;
        };
        if !alive(process) {
            let _ = std::fs::write(folder.join(FAILED), format!("{process}\n"));
            let _ = std::fs::remove_file(entry.path());
            return true;
        }
    }
    false
}

/// One program's trial of the chip, from before it starts the GPU.
pub(crate) struct Trial {
    file: PathBuf,
    frames: u32,
}

impl Trial {
    /// Say this process is trying. `None` when it can't be written down:
    /// the program draws on the chip all the same, with nothing to tell
    /// the next one if it goes wrong.
    pub(crate) fn begin(folder: &Path) -> Option<Trial> {
        let file = folder.join(format!("{TRYING}{}", std::process::id()));
        std::fs::write(&file, b"").ok()?;
        Some(Trial { file, frames: 0 })
    }

    /// A frame was handed over. True once there have been enough: the
    /// trial is passed, and is dropped.
    pub(crate) fn frame(&mut self) -> bool {
        self.frames += 1;
        self.frames >= ENOUGH
    }
}

impl Drop for Trial {
    /// Passed, or the program is closing in order: either way it did not
    /// end badly.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("shine-trial-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn a_trial_that_shows_its_frames_leaves_nothing_behind() {
        let dir = folder("passed");
        let mut trial = Trial::begin(&dir).expect("the folder can be written to");
        // While it is trying, another program takes the chip too.
        assert!(!failed(&dir));
        assert!(!trial.frame());
        assert!(!trial.frame());
        assert!(trial.frame());
        drop(trial);
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        assert!(!failed(&dir));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_program_that_ended_while_trying_keeps_the_rest_off_the_chip() {
        let dir = folder("ended");
        // Left by a process that is no longer there, and by one that is.
        std::fs::write(dir.join(format!("{TRYING}41")), b"").unwrap();
        std::fs::write(dir.join(format!("{TRYING}42")), b"").unwrap();
        assert!(!failed_by(&dir, |_| true));
        assert!(failed_by(&dir, |process| process == 42));
        // Said once, and known from then on whoever asks.
        assert!(dir.join(FAILED).exists());
        assert!(failed_by(&dir, |_| true));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_program_that_closes_in_order_did_not_end_badly() {
        let dir = folder("closed");
        let trial = Trial::begin(&dir).expect("the folder can be written to");
        drop(trial);
        assert!(!failed_by(&dir, |_| false));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn other_files_in_the_folder_say_nothing() {
        let dir = folder("others");
        std::fs::write(dir.join("shine-gpu-trial-notes"), b"").unwrap();
        std::fs::write(dir.join("something-else"), b"").unwrap();
        assert!(!failed_by(&dir, |_| false));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
