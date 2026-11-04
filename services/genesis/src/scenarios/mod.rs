//! One module per family of scenarios; `run` dispatches on the assignment.

mod faults;
mod flood;
mod lifecycle;
mod pair;
mod rpc;
mod validate;

use foundation::stress::Scenario;

use crate::bootstrap::Assignment;

pub fn run(assignment: &Assignment, record: &mut [u8]) {
    match assignment.scenario {
        Scenario::PingPong => pair::ping_pong(assignment, record),
        Scenario::CollectorSender => flood::collector_sender(assignment),
        Scenario::Collector => flood::collector(assignment, record),
        Scenario::DeadLetter => flood::dead_letter(assignment),
        Scenario::FillVictim => flood::fill_victim(assignment),
        Scenario::Server => rpc::server(assignment, record),
        Scenario::Client => rpc::client(assignment, record),
        Scenario::ClientCrash => rpc::client_crash(assignment),
        Scenario::SelfCycle => validate::self_cycle(assignment),
        Scenario::SizeProbe => validate::size_probe(),
        Scenario::SystemCallFuzz => validate::system_call_fuzz(assignment),
        Scenario::QuickExit => {}
        Scenario::SpinHog => lifecycle::spin_hog(assignment),
        Scenario::BlockForever => lifecycle::block_forever(record),
        Scenario::CrashBreakpoint => faults::breakpoint(),
        Scenario::CrashNullWrite => faults::null_write(),
        Scenario::CrashExecuteData => faults::execute_data(),
        Scenario::CrashUndefined => faults::undefined_instruction(),
        Scenario::CrashPanic => faults::panic(),
        Scenario::CrashStackOverflow => faults::stack_overflow(),
        Scenario::CrashWriteCode => faults::write_code(),
    }
}
