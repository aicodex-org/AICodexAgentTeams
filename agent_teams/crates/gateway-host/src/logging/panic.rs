//! 将进程 panic 的安全位置写入日志，不展开可能携带凭据的 payload

use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn install() {
    let reported = AtomicBool::new(false);
    std::panic::set_hook(Box::new(move |info| {
        let (file, line, column) = info.location().map_or(("unknown", 0, 0), |location| {
            (location.file(), location.line(), location.column())
        });
        tracing::error!(
            target: "gateway_panic",
            event = "process.panic",
            file,
            line,
            column,
            "线程发生 panic，原始内容已省略"
        );
        if !reported.swap(true, Ordering::Relaxed) {
            // 默认 hook 会原样输出 payload；使用安全兜底且不让 stderr 阻塞异常线程
            let message =
                format!("gateway_panic: process.panic at {file}:{line}:{column}; payload omitted");
            let _ = std::thread::Builder::new()
                .name("gateway-panic-fallback".to_owned())
                .spawn(move || eprintln!("{message}"));
        }
    }));
}
