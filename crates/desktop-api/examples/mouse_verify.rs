//! mouse_verify — 验证 move_to 修复：真实移动或明确报错（不再静默成功）
//!
//! 运行：cargo run -p desktop-api --example mouse_verify

use desktop_api::input::mouse;

#[tokio::main]
async fn main() {
    // 1. 当前鼠标位置
    match mouse::position().await {
        Ok(p) => println!("[1] 当前位置: ({}, {})", p.x, p.y),
        Err(e) => {
            println!("[1] position 失败: {e}");
            return;
        }
    }

    // 2. 移动到目标坐标（屏幕中部，避免误点）
    let target = (500, 400);
    println!("[2] move_to({}, {}) ...", target.0, target.1);
    match mouse::move_to(target.0, target.1).await {
        Ok(()) => {
            // 3. 移动后读实际位置，验证是否真的到达
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            match mouse::position().await {
                Ok(p) => {
                    println!("[3] 移动后实际位置: ({}, {})", p.x, p.y);
                    let dx = (p.x - target.0).abs();
                    let dy = (p.y - target.1).abs();
                    if dx <= 2 && dy <= 2 {
                        println!("✅ 验证通过：鼠标真实移动到目标 (容差内)");
                    } else {
                        println!("❌ 验证失败：move_to 返回成功但实际位置偏差 ({dx},{dy})px");
                    }
                }
                Err(e) => println!("[3] position 失败: {e}"),
            }
        }
        Err(e) => {
            println!("[2] move_to 明确报错（修复生效，不再静默成功）: {e}");
            println!("说明：环境拦截了鼠标移动（SetCursorPos 失败或移动后校验不符）");
        }
    }
}
