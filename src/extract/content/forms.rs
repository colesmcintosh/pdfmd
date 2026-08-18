//! Budgets for Form XObject execution.
//!
//! A page's Form graph can branch, so a handful of small Forms can expand
//! into unbounded work. Every invocation, every input byte interpreted, and
//! every output byte produced inside a Form is metered here; the interpreter
//! asks before entering a Form and charges as it emits.

use crate::pdf::ObjectId;

/// Bound recursive invocation independently of the resource graph pre-pass.
/// Real documents rarely nest Forms more than a few levels; this cap keeps
/// adversarial acyclic chains from exhausting the stack.
const MAX_FORM_DEPTH: usize = 32;

const MAX_FORM_INVOCATIONS_PER_PAGE: usize = 16_384;
const MAX_FORM_INPUT_BYTES_PER_PAGE: usize = 64 * 1024 * 1024;
const MAX_FORM_OUTPUT_BYTES_PER_PAGE: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(super) struct FormExecutionLimits {
    pub(super) invocations: usize,
    pub(super) input_bytes: usize,
    pub(super) output_bytes: usize,
}

pub(super) const FORM_EXECUTION_LIMITS: FormExecutionLimits = FormExecutionLimits {
    invocations: MAX_FORM_INVOCATIONS_PER_PAGE,
    input_bytes: MAX_FORM_INPUT_BYTES_PER_PAGE,
    output_bytes: MAX_FORM_OUTPUT_BYTES_PER_PAGE,
};

struct ActiveForm {
    id: ObjectId,
    /// Bytes below this point belong to the caller and must remain untouched.
    output_floor: usize,
}

/// The Form call stack plus what it has spent so far.
pub(super) struct FormExecution {
    limits: FormExecutionLimits,
    active: Vec<ActiveForm>,
    invocations: usize,
    input_bytes: usize,
    output_bytes: usize,
}

impl FormExecution {
    pub(super) fn new(limits: FormExecutionLimits) -> Self {
        Self {
            limits,
            active: Vec::new(),
            invocations: 0,
            input_bytes: 0,
            output_bytes: 0,
        }
    }

    /// Are we inside a Form? Only Form-produced output is charged, so page
    /// content itself is never truncated.
    pub(super) fn is_nested(&self) -> bool {
        !self.active.is_empty()
    }

    pub(super) fn output_floor(&self) -> usize {
        self.active
            .last()
            .map(|form| form.output_floor)
            .unwrap_or(0)
    }

    pub(super) fn output_exhausted(&self) -> bool {
        self.output_bytes >= self.limits.output_bytes
    }

    pub(super) fn output_remaining(&self) -> usize {
        self.limits.output_bytes.saturating_sub(self.output_bytes)
    }

    /// Output charged so far, so a caller can measure what one operator added
    /// beyond what the Forms it invoked already charged.
    pub(super) fn charged(&self) -> usize {
        self.output_bytes
    }

    /// Charge `added` output bytes, truncating `out` back to the ceiling when
    /// the operator overshot it.
    pub(super) fn charge_output(&mut self, out: Option<&mut String>, added: usize) {
        let remaining = self.output_remaining();
        if added <= remaining {
            self.output_bytes += added;
            return;
        }
        if let Some(out) = out {
            let mut new_len = out.len().saturating_sub(added - remaining);
            while new_len > 0 && !out.is_char_boundary(new_len) {
                new_len -= 1;
            }
            out.truncate(new_len);
        }
        self.output_bytes = self.limits.output_bytes;
    }

    /// Enter Form `id`, charging `input_len`. Refuses when the call would
    /// nest too deep, re-enter a Form already on the stack (an invocation
    /// cycle), or overrun any budget.
    pub(super) fn try_enter(
        &mut self,
        id: ObjectId,
        input_len: usize,
        output_floor: usize,
    ) -> bool {
        if self.active.len() >= MAX_FORM_DEPTH
            || self.active.iter().any(|form| form.id == id)
            || self.invocations >= self.limits.invocations
            || self.output_exhausted()
            || input_len > self.limits.input_bytes.saturating_sub(self.input_bytes)
        {
            return false;
        }
        self.invocations += 1;
        self.input_bytes += input_len;
        self.active.push(ActiveForm { id, output_floor });
        true
    }

    pub(super) fn leave(&mut self) {
        self.active.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlimited(output_bytes: usize) -> FormExecutionLimits {
        FormExecutionLimits {
            invocations: usize::MAX,
            input_bytes: usize::MAX,
            output_bytes,
        }
    }

    #[test]
    fn try_enter_refuses_cycles_depth_and_budgets() {
        let mut exec = FormExecution::new(unlimited(usize::MAX));
        assert!(!exec.is_nested());
        assert!(exec.try_enter(ObjectId(1, 0), 0, 7));
        assert!(exec.is_nested());
        assert_eq!(exec.output_floor(), 7);
        // Re-entering an active Form is an invocation cycle.
        assert!(!exec.try_enter(ObjectId(1, 0), 0, 0));
        exec.leave();
        assert_eq!(exec.output_floor(), 0);

        // Depth cap.
        let mut exec = FormExecution::new(unlimited(usize::MAX));
        for n in 0..MAX_FORM_DEPTH as u32 {
            assert!(exec.try_enter(ObjectId(n, 0), 0, 0), "{n}");
        }
        assert!(!exec.try_enter(ObjectId(999, 0), 0, 0));

        // Invocation, input, and output ceilings each refuse on their own.
        let mut exec = FormExecution::new(FormExecutionLimits {
            invocations: 1,
            ..unlimited(usize::MAX)
        });
        assert!(exec.try_enter(ObjectId(1, 0), 0, 0));
        exec.leave();
        assert!(!exec.try_enter(ObjectId(2, 0), 0, 0));

        let mut exec = FormExecution::new(FormExecutionLimits {
            input_bytes: 4,
            ..unlimited(usize::MAX)
        });
        assert!(!exec.try_enter(ObjectId(1, 0), 5, 0));
        assert!(exec.try_enter(ObjectId(1, 0), 4, 0));

        let mut exec = FormExecution::new(unlimited(2));
        exec.charge_output(None, 2);
        assert!(exec.output_exhausted());
        assert!(!exec.try_enter(ObjectId(1, 0), 0, 0));
    }

    #[test]
    fn charge_output_truncates_on_a_char_boundary() {
        let mut exec = FormExecution::new(unlimited(4));
        let mut out = String::from("ab");
        exec.charge_output(Some(&mut out), 2);
        assert_eq!((exec.charged(), exec.output_remaining()), (2, 2));

        // "é" is two bytes; overshooting must not split it.
        out.push_str("cé");
        exec.charge_output(Some(&mut out), 3);
        assert_eq!(out, "abc");
        assert!(exec.output_exhausted());
    }
}
