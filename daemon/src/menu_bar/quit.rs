//! The "Quit turbofig" sequence: stop the daemon, wait briefly until
//! `/health` stops answering, then exit the app. `DaemonStopper` is the seam:
//! `quit_sequence` itself is pure control flow with no network or process
//! dependency, tested with a fake that records call order; `mod.rs`'s real
//! implementation is the only thing that ever talks to a real daemon.

/// Stops the running daemon and confirms it is gone. `RealDaemonStopper`
/// (`mod.rs`) does this over HTTP (`POST /control` with the pairing token,
/// then polling `/health`); a test uses a fake instead.
pub trait DaemonStopper {
    /// Issues the stop request. Returns whether it was accepted; a quit
    /// proceeds to wait regardless, the same way `cmd_stop` does; a daemon
    /// that was already gone is not an error.
    fn stop(&self) -> bool;
    /// Waits (up to the stopper's own deadline) for `/health` to stop
    /// answering. Returns whether it actually went unreachable.
    fn wait_unreachable(&self) -> bool;
}

/// Runs the quit sequence: `stop`, then `wait_unreachable`, in that order.
/// Returns `wait_unreachable`'s result, which `mod.rs` uses to decide
/// whether to log a warning before exiting; it exits either way, since a
/// user clicking Quit wants the app gone now, not a daemon that outlives it
/// by a few extra seconds under exceptional circumstances.
pub fn quit_sequence(stopper: &dyn DaemonStopper) -> bool {
    stopper.stop();
    stopper.wait_unreachable()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeStopper {
        calls: RefCell<Vec<&'static str>>,
        stop_result: bool,
        wait_result: bool,
    }

    impl DaemonStopper for FakeStopper {
        fn stop(&self) -> bool {
            self.calls.borrow_mut().push("stop");
            self.stop_result
        }
        fn wait_unreachable(&self) -> bool {
            self.calls.borrow_mut().push("wait_unreachable");
            self.wait_result
        }
    }

    #[test]
    fn calls_stop_before_wait_unreachable() {
        let stopper = FakeStopper {
            calls: RefCell::new(Vec::new()),
            stop_result: true,
            wait_result: true,
        };
        quit_sequence(&stopper);
        assert_eq!(*stopper.calls.borrow(), vec!["stop", "wait_unreachable"]);
    }

    #[test]
    fn returns_wait_unreachables_result_when_true() {
        let stopper = FakeStopper {
            calls: RefCell::new(Vec::new()),
            stop_result: true,
            wait_result: true,
        };
        assert!(quit_sequence(&stopper));
    }

    #[test]
    fn returns_wait_unreachables_result_when_false() {
        let stopper = FakeStopper {
            calls: RefCell::new(Vec::new()),
            stop_result: true,
            wait_result: false,
        };
        assert!(!quit_sequence(&stopper));
    }

    #[test]
    fn still_waits_even_when_stop_itself_reports_failure() {
        let stopper = FakeStopper {
            calls: RefCell::new(Vec::new()),
            stop_result: false,
            wait_result: true,
        };
        assert!(quit_sequence(&stopper));
        assert_eq!(*stopper.calls.borrow(), vec!["stop", "wait_unreachable"]);
    }
}
