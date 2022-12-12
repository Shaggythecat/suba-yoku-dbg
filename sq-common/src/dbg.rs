use std::{time::Duration, sync::{Arc, mpsc}};
use anyhow::{Result, bail};
use atomic::{Atomic, Ordering};
use crate::sq::*;

const RECV_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Copy, Clone, PartialEq, PartialOrd, Eq, Ord, Debug, Hash)]
pub enum ExecState {
    Running,
    Halted
}

pub enum DebugMsg {
    Step,
    Backtrace,
    Locals(Option<SqUnsignedInteger>)
}

#[derive(Clone, PartialEq, PartialOrd, Eq, Ord, Debug, Hash)]
pub struct EventWithSrc {
    pub event: DebugEvent,
    pub src: Option<String>
}

/// SqLocalVar annotated with level
#[derive(Clone, PartialEq, PartialOrd, Eq, Debug, Hash)]
pub struct SqLocalVarWithLvl {
    pub var: SqLocalVar,
    pub lvl: SqUnsignedInteger,
}

impl std::fmt::Display for EventWithSrc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let src = if let Some(src_f) = &self.src { src_f } else { "??" };
        match &self.event {
            DebugEvent::Line(line) => write!(f, "line: {src}:{line}"),
            DebugEvent::FnCall(name, line) => 
                write!(f, 
                    "call: {src}:{name} ({ln})",
                    ln = if let Some(line) = line { line.to_string() } else { "??".into() }
                ),
            DebugEvent::FnRet(name, line) =>                 
                write!(f, 
                    "ret:  {src}:{name} ({ln})",
                    ln = if let Some(line) = line { line.to_string() } else { "??".into() }
                ),
        }
    }
}

pub type SqBacktrace = Vec<SqStackInfo>;

#[derive(Clone, PartialEq, PartialOrd, Eq, Debug, Hash)]
pub enum DebugResp {
    Event(EventWithSrc),
    Backtrace(SqBacktrace),
    Locals(Option<Vec<SqLocalVarWithLvl>>)
}

pub struct SqDebugger<'a>{
    exec_state: Arc<Atomic<ExecState>>,
    sender: mpsc::Sender<DebugMsg>,
    receiver: mpsc::Receiver<DebugResp>,
    vm: SafeVm<'a>,
}



impl<'a> SqDebugger<'a>
{
    /// Attach debugger to SQVM through setting debug hook.
    pub fn attach(vm: SafeVm<'a>) -> SqDebugger<'a> {

        let (tx, rx) = mpsc::channel();
        let (resp_tx, resp_rx) = mpsc::sync_channel(0);

        let mut dbg = Self {
            exec_state: Arc::new(Atomic::new(ExecState::Halted)),
            sender: tx,
            receiver: resp_rx,
            vm,
        };

        let exec_state = dbg.exec_state.clone();

        // Attached debugger will receive messages and respond to them
        dbg.vm.set_debug_hook(Box::new(move |e, src, vm| {

            // Vm was halted or step cmd was received on previous debug hook call
            // So send debug event back
            // This will block until msg isn`t received
            if exec_state.load(Ordering::Relaxed) == ExecState::Halted {
                resp_tx.send(DebugResp::Event(EventWithSrc {
                    event: e,
                    src
                })).unwrap();
            }
            
            loop {
                if let Ok(msg) = rx.try_recv() { match msg {
                    // Expected immediate receive on other end for all sending cmds

                    DebugMsg::Step => break,
                    DebugMsg::Backtrace => {
                        let mut bt = vec![];
                        let mut lvl = 1;
                        while let Ok(info) = vm.get_stack_info(lvl) {
                            bt.push(info);
                            lvl += 1;
                        }
                        resp_tx.send(DebugResp::Backtrace(bt)).unwrap();
                    },
                    DebugMsg::Locals(lvl_opt) => {
                        let mut v = vec![];

                        // Store all locals if level isn`t specified  
                        let mut lvl = if let Some(lvl) = lvl_opt { lvl } else { 1 };

                        // Try to get zeroth local
                        while let Ok(loc) = vm.get_local(lvl, 0) {
                            v.push(SqLocalVarWithLvl { var: loc, lvl });

                            let mut idx = 1;
                            while let Ok(loc) = vm.get_local(lvl, idx) {
                                v.push(SqLocalVarWithLvl { var: loc, lvl });
                                idx += 1;
                            }

                            if lvl_opt.is_some() {
                                break;
                            } else {
                                lvl += 1;
                            }
                        }

                        resp_tx.send(DebugResp::Locals(if v.is_empty() { None } else { Some(v) })).unwrap();
                    },
                }}

                if exec_state.load(Ordering::Relaxed) == ExecState::Running {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }));

        dbg
    }

    /// Resume execution
    pub fn resume(&self) {
        self.exec_state.store(ExecState::Running, Ordering::Relaxed);
    }

    /// Get internal message receiver
    /// 
    /// May be useful to manage complicated states such as getting message from suspended vm thread 
    pub fn receiver(&self) -> &mpsc::Receiver<DebugResp> {
        &self.receiver
    }

    /// Halt execution by blocking vm on debug hook call
    /// 
    /// Returns last received event if `no_recv` is `false`
    pub fn halt(&self, no_recv: bool) -> Result<Option<EventWithSrc>> {
        let prev = self.exec_state.swap(ExecState::Halted, Ordering::Relaxed);
        
        if prev == ExecState::Running && !no_recv {
            match self.receiver.recv_timeout(RECV_TIMEOUT) {
                Ok(DebugResp::Event(e)) => Ok(Some(e)),
                Ok(r) => bail!("{r:?}: expected event"),
                Err(e) => bail!("{e}")
            }
        } else { Ok(None) }
    }

    /// Unlock current debug hook call
    /// 
    /// Returns received event
    pub fn step(&self) -> Result<EventWithSrc> {
        self.sender.send(DebugMsg::Step)?;
        match self.receiver.recv_timeout(RECV_TIMEOUT) {
            Ok(DebugResp::Event(e)) => Ok(e),
            Ok(r) => bail!("{r:?}: expected event"),
            Err(e) => bail!("{e}")
        }
    }

    /// Get local variables and their values at specified level.
    /// 
    /// May be pretty expensive
    /// 
    /// If `lvl` is `None`, return locals gathered from all levels
    pub fn get_locals(&self, lvl: Option<SqUnsignedInteger>) -> Result<Vec<SqLocalVarWithLvl>> {
        self.sender.send(DebugMsg::Locals(lvl))?;
        match self.receiver.recv_timeout(RECV_TIMEOUT) {
            Ok(DebugResp::Locals(loc)) => 
                if let Some(loc) = loc {
                    Ok(loc)
                } else { match lvl {
                    Some(lvl) => bail!("no locals at level {lvl}"),
                    None => bail!("no locals at all levels"),
                }},
            Ok(r) => bail!("{r:?}: expected locals"),
            Err(e) => bail!("{e}")
        }
    }

    /// Request backtrace from vm thread, where
    /// ```text
    /// vec_start     vec_end
    /// ^^^^^^^^^     ^^^^
    /// current_fn -> root
    /// ```
    pub fn get_backtrace(&self) -> Result<SqBacktrace> {
        self.sender.send(DebugMsg::Backtrace)?;
        match self.receiver.recv_timeout(RECV_TIMEOUT) {
            Ok(DebugResp::Backtrace(bt)) => Ok(bt),
            Ok(r) => bail!("{r:?}: expected backtrace"),
            Err(e) => bail!("{e}")
        }
    }

    pub fn exec_state(&self) -> ExecState {
        self.exec_state.load(Ordering::Relaxed)
    }
}
