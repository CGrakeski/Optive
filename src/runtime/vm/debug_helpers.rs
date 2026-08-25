use super::{ErrorStackFrame, Vm};
use crate::shared::Shared;

impl Vm {
    pub(crate) fn line_from_map(line_map: &[usize], pc: usize) -> usize {
        if pc == 0 {
            return line_map.first().copied().unwrap_or(0);
        }
        line_map.get(pc.saturating_sub(1)).copied().unwrap_or(0)
    }

    pub(crate) fn current_column(&self) -> usize {
        if self.pc == 0 {
            return self.active_column_map.first().copied().unwrap_or(1);
        }
        self.active_column_map
            .get(self.pc.saturating_sub(1))
            .copied()
            .unwrap_or(1)
    }

    #[inline]
    pub const fn debug_call_depth(&self) -> usize {
        self.user_call_frames.len() + self.lw_depth
    }

    pub fn debug_current_func_name(&self) -> Option<String> {
        self.func_stack.last().map(|f| f.name.clone())
    }

    /// 当前调度任务（主纤程为 `None`）。
    pub fn debug_current_task(&self) -> Option<Shared<crate::value::TaskInner>> {
        self.task_ctx.as_ref().map(|c| c.task.clone())
    }

    pub fn debug_build_stack_frames(&self) -> Vec<ErrorStackFrame> {
        let mut frames = Vec::new();
        for (i, ucf) in self.user_call_frames.iter().enumerate() {
            let (func, file, source) = if i == 0 {
                (
                    "<module>".to_string(),
                    self.source_file.clone(),
                    self.current_source.clone(),
                )
            } else {
                let caller = &self.user_call_frames[i - 1].func;
                (
                    caller.name.clone(),
                    caller.source_file.clone(),
                    caller.source.clone(),
                )
            };
            frames.push(ErrorStackFrame {
                func,
                file,
                line: Self::line_from_map(&ucf.saved_line_map, ucf.saved_pc),
                column: Self::line_from_map(&ucf.saved_column_map, ucf.saved_pc).max(1),
                source,
            });
        }
        let (func, file, source) = if let Some(f) = self.func_stack.last() {
            (f.name.clone(), f.source_file.clone(), f.source.clone())
        } else {
            (
                "<module>".to_string(),
                self.source_file.clone(),
                self.current_source.clone(),
            )
        };
        frames.push(ErrorStackFrame {
            func,
            file,
            line: crate::debug::line_at_pc(self),
            column: crate::debug::column_at_pc(self).max(1),
            source,
        });
        frames
    }
}
