//! 光标动画状态机：1:1 移植自 C# MainWindow.cs 的渲染/动画逻辑。
//! 弹簧、elastic 回弹、拖动旋转、按下缩放/发光等。

use crate::log::log;

/// 基础尺寸常量（与 C# 一致）。
pub const BASE_CURSOR_WIDTH: f64 = 30.0;
pub const BASE_CURSOR_HEIGHT: f64 = 42.5;
pub const POINTER_ANGLE: f64 = 24.3;
pub const BASE_WINDOW_SIZE: f64 = 160.0;
pub const BASE_WINDOW_MARGIN: f64 = 64.0;
pub const MIN_CURSOR_WIDTH: f64 = 16.0;
pub const MAX_CURSOR_WIDTH: f64 = 64.0;
const SETTLE_POSITION_EPSILON: f64 = 0.0008;
const SETTLE_VELOCITY_EPSILON: f64 = 0.01;
// 软件合成器的帧间观感比 WPF 的原版尾巴更拖。保留原版 elastic 曲线，
// 只缩短时间基数，让释放具有更明确的回弹冲量。
const ELASTIC_BASE_DURATION: f64 = 0.42;
/// 拖拽时角度追随速率。越大光标越跟手，且快速画圈时角度累积越充分
/// （模拟：rate=8 时 2500°/s 快速画圈仅累积 131° 无法转圈；rate=16 达 858°）。
/// 原版为 8.0，此处取 16.0 以保证快速画圈后释放也能明显转圈。
const DRAG_FOLLOW_RATE: f64 = 16.0;

/// 由光标宽度推导的几何尺寸。
#[derive(Debug, Clone, Copy)]
pub struct CursorGeometry {
    pub cursor_width: f64,
    pub cursor_height: f64,
    pub window_size: f64,
    pub window_margin: f64,
}

pub fn geometry_for_width(width: f64) -> CursorGeometry {
    let w = width.clamp(MIN_CURSOR_WIDTH, MAX_CURSOR_WIDTH);
    CursorGeometry {
        cursor_width: w,
        cursor_height: w * (BASE_CURSOR_HEIGHT / BASE_CURSOR_WIDTH),
        window_size: w * (BASE_WINDOW_SIZE / BASE_CURSOR_WIDTH),
        window_margin: w * (BASE_WINDOW_MARGIN / BASE_CURSOR_WIDTH),
    }
}

/// elastic-out 缓动（与原版 MainWindow.cs 一致）。
/// 转圈幅度来自拖动阶段累计得到的 start_angle；这个函数只负责回弹，
/// 不通过修改频率或衰减参数人为放大角度。
pub fn elastic_out(t: f64) -> f64 {
    (2.0f64).powf(-10.0 * t) * ((0.5 * t - 0.075) * 20.943951023931955).sin() + 1.0
        - 0.0004882812499999998 * t
}

/// 将角度归一化到 [-180, 180]。
pub fn normalize_angle(degrees: f64) -> f64 {
    let mut d = degrees % 360.0;
    if d > 180.0 {
        d -= 360.0;
    } else if d < -180.0 {
        d += 360.0;
    }
    d
}

/// 拖动时的目标角度。
pub fn drag_angle(dx: f64, dy: f64) -> f64 {
    (-dx).atan2(dy).to_degrees() + POINTER_ANGLE
}

/// 光标动画状态。
#[derive(Debug, Clone, Copy)]
pub struct CursorAnim {
    pub angle: f64,
    pub angle_velocity: f64,
    pub scale_value: f64,
    pub scale_velocity: f64,
    pub additive_opacity: f64,
    pub opacity_velocity: f64,

    pub mouse_down: bool,
    pub drag_active: bool,
    pub pointer_hover: bool,
    /// 鼠标停在可调整大小的窗口边框时的静止朝向；按下后立即清空，
    /// 让现有拖动/回弹旋转逻辑接管。
    pub resize_angle: Option<f64>,
    pub spin_returning: bool,
    pub elastic_returning: bool,
    spin_elapsed: f64,
    spin_duration: f64,
    spin_start_angle: f64,
    spin_end_angle: f64,
    elastic_start_angle: f64,
    elastic_duration: f64,
    elastic_elapsed: f64,
    // 拖动方向不能直接把每帧 atan2 的结果当作最终角度：跨过 ±180°
    // 时必须保留连续变化，否则绕圈会被折叠成一次很小的摆动。
    drag_target_angle: f64,
    last_drag_raw_angle: f64,
    drag_angle_initialized: bool,
}

impl Default for CursorAnim {
    fn default() -> Self {
        Self {
            angle: 0.0,
            angle_velocity: 0.0,
            scale_value: 1.0,
            scale_velocity: 0.0,
            additive_opacity: 0.0,
            opacity_velocity: 0.0,
            mouse_down: false,
            drag_active: false,
            pointer_hover: false,
            resize_angle: None,
            spin_returning: false,
            elastic_returning: false,
            spin_elapsed: 0.0,
            spin_duration: 0.5,
            spin_start_angle: 0.0,
            spin_end_angle: 0.0,
            elastic_start_angle: 0.0,
            elastic_duration: ELASTIC_BASE_DURATION,
            elastic_elapsed: 0.0,
            drag_target_angle: 0.0,
            last_drag_raw_angle: 0.0,
            drag_angle_initialized: false,
        }
    }
}

impl CursorAnim {
    /// 判断这一帧是否真的需要重新栅格化。光标移动时只移动分层窗口，
    /// 不重复绘制相同位图，避免空闲状态持续占用 CPU。
    pub fn visual_changed_from(&self, previous: &Self) -> bool {
        (self.angle - previous.angle).abs() > 0.0001
            || (self.scale_value - previous.scale_value).abs() > 0.0001
            || (self.additive_opacity - previous.additive_opacity).abs() > 0.0001
    }

    /// 按下：开始缩放/发光，准备拖动。
    pub fn begin_press(&mut self) {
        self.spin_returning = false;
        self.elastic_returning = false;
        self.resize_angle = None;
        self.mouse_down = true;
        self.drag_active = false;
        self.drag_target_angle = self.angle;
        self.last_drag_raw_angle = 0.0;
        self.drag_angle_initialized = false;
    }

    /// 抬起：若曾拖动则先旋转 3 圈，再弹性回归初始状态。
    pub fn end_press(&mut self) {
        if !self.mouse_down {
            return;
        }
        if self.drag_active {
            self.start_release_animation();
        }
        self.mouse_down = false;
        self.drag_active = false;
    }

    /// 释放动画入口：先 0.5s 旋转 3 圈（spin），再衔接 elastic 摆动回归。
    fn start_release_animation(&mut self) {
        if self.angle.abs() < 0.5 {
            return;
        }
        self.spin_returning = true;
        self.spin_elapsed = 0.0;
        self.spin_duration = 0.5;
        self.spin_start_angle = self.angle;
        self.spin_end_angle = self.angle + 3.0 * 360.0;
        self.angle_velocity = 0.0;
        log(&format!(
            "release spin: start={:.1} end={:.1} duration={:.2}s",
            self.spin_start_angle, self.spin_end_angle, self.spin_duration
        ));
    }

    /// 旋转 3 圈阶段：quad ease-out 推进，结束后衔接 elastic 回弹。
    fn update_spin(&mut self, dt: f64) {
        self.spin_elapsed += dt;
        let t = (self.spin_elapsed / self.spin_duration).min(1.0);
        let progress = 1.0 - (1.0 - t) * (1.0 - t);
        self.angle = self.spin_start_angle
            + (self.spin_end_angle - self.spin_start_angle) * progress;
        // 用 elapsed >= duration 判定（浮点累加可能略小于 1.0 的 t）。
        if self.spin_elapsed >= self.spin_duration {
            self.spin_returning = false;
            // 正转 3 圈（1080°）后朝向 = 归一化角度；用归一化值做小幅弹性回弹，
            // 避免 elastic 从绝对大角度倒转 3.5 圈。1280° 与 -160° 视觉等价，无跳变。
            let normalized = normalize_angle(self.spin_end_angle);
            self.angle = normalized;
            self.start_elastic_return_from(normalized);
        }
    }

    fn start_elastic_return_from(&mut self, start_angle: f64) {
        if start_angle.abs() < 0.5 {
            return;
        }
        self.elastic_start_angle = start_angle;
        self.elastic_duration = ELASTIC_BASE_DURATION * (1.0 + (start_angle / 720.0).abs());
        self.elastic_elapsed = 0.0;
        self.elastic_returning = true;
        self.angle_velocity = 0.0;
        log(&format!(
            "elastic release: start_angle={:.1} duration={:.2}s",
            self.elastic_start_angle, self.elastic_duration
        ));
    }

    fn update_elastic_return(&mut self, dt: f64) {
        self.elastic_elapsed += dt;
        let t = (self.elastic_elapsed / self.elastic_duration).min(1.0);
        self.angle = self.elastic_start_angle * (1.0 - elastic_out(t));
        if t >= 1.0 {
            self.angle = 0.0;
            self.elastic_returning = false;
            self.angle_velocity = 0.0;
        }
    }

    fn settle(value: &mut f64, velocity: &mut f64, target: f64) {
        if (*value - target).abs() < SETTLE_POSITION_EPSILON
            && velocity.abs() < SETTLE_VELOCITY_EPSILON
        {
            *value = target;
            *velocity = 0.0;
        }
    }

    /// 返回连续展开后的拖动目标角度。
    ///
    /// `atan2` 本身只返回一个主值区间。直接使用它会在分支边界把
    /// `179° -> -179°` 当成一次大跳变；原版通过 NormalizeAngle 逐帧
    /// 追随时可以保留这次跨界，但 Rust 版如果只保存当前目标角度，
    /// 在更高/不规则的定时器采样下很容易丢掉绕圈累计量。因此这里
    /// 显式累积相邻方向的最短角差，再让实际角度追随这个未归一化目标。
    fn update_drag_target(&mut self, dx: f64, dy: f64) -> f64 {
        let raw_angle = drag_angle(dx, dy);
        if !self.drag_angle_initialized {
            // 第一次激活时仍选择离当前光标最近的等价角度，避免按下后
            // 因 atan2 主值区间产生一次反向跳转。
            self.drag_target_angle = self.angle + normalize_angle(raw_angle - self.angle);
            self.last_drag_raw_angle = raw_angle;
            self.drag_angle_initialized = true;
        } else {
            self.drag_target_angle += normalize_angle(raw_angle - self.last_drag_raw_angle);
            self.last_drag_raw_angle = raw_angle;
        }
        self.drag_target_angle
    }

    /// 更新一帧（dt 秒）。
    pub fn update(&mut self, dt: f64, drag_dx: f64, drag_dy: f64) {
        let (target_scale, target_additive, new_angle) = if self.mouse_down {
            // 按下：缩小 + 发光；拖动时按拖动方向旋转
            let target_angle = if self.drag_active {
                self.update_drag_target(drag_dx, drag_dy)
            } else {
                0.0
            };
            // target_angle 已经是连续展开角度，这里不能再次归一化，
            // 否则累计超过一圈后会重新折回 [-180°, 180°]。
            let delta = target_angle - self.angle;
            let a = self.angle + delta * (dt * DRAG_FOLLOW_RATE).clamp(0.0, 1.0);
            (0.9, 1.0, a)
        } else if self.spin_returning {
            self.update_spin(dt);
            (1.0, 0.0, self.angle)
        } else if self.elastic_returning {
            self.update_elastic_return(dt);
            (1.0, 0.0, self.angle)
        } else {
            // 边框方向优先于普通链接悬停；没有边框方向时保留原版手型角度。
            let target_angle = self.resize_angle.unwrap_or_else(|| {
                if self.pointer_hover {
                    POINTER_ANGLE
                } else {
                    0.0
                }
            });
            let delta = normalize_angle(target_angle - self.angle);
            self.angle_velocity += (240.0 * delta - 20.0 * self.angle_velocity) * dt;
            let a = self.angle + self.angle_velocity * dt;
            (1.0, if self.pointer_hover { 1.0 } else { 0.0 }, a)
        };
        self.angle = new_angle;

        self.scale_velocity +=
            (240.0 * (target_scale - self.scale_value) - 20.0 * self.scale_velocity) * dt;
        self.scale_value += self.scale_velocity * dt;
        self.opacity_velocity +=
            (160.0 * (target_additive - self.additive_opacity) - 18.0 * self.opacity_velocity) * dt;
        self.additive_opacity += self.opacity_velocity * dt;

        self.scale_value = self.scale_value.clamp(0.8, 1.1);
        self.additive_opacity = self.additive_opacity.clamp(0.0, 1.0);

        let target_angle = if self.mouse_down {
            if self.drag_active {
                self.drag_target_angle
            } else {
                0.0
            }
        } else if !self.elastic_returning && !self.spin_returning {
            self.resize_angle.unwrap_or_else(|| {
                if self.pointer_hover {
                    POINTER_ANGLE
                } else {
                    0.0
                }
            })
        } else {
            self.angle
        };
        if !self.elastic_returning && !self.spin_returning {
            Self::settle(&mut self.angle, &mut self.angle_velocity, target_angle);
        }
        Self::settle(
            &mut self.scale_value,
            &mut self.scale_velocity,
            target_scale,
        );
        Self::settle(
            &mut self.additive_opacity,
            &mut self.opacity_velocity,
            target_additive,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::CursorAnim;

    #[test]
    fn idle_animation_settles_without_perpetual_drift() {
        let mut anim = CursorAnim::default();
        for _ in 0..120 {
            anim.update(1.0 / 60.0, 0.0, 0.0);
        }
        let settled = anim;
        anim.update(1.0 / 60.0, 0.0, 0.0);
        assert_eq!(anim.angle, 0.0);
        assert_eq!(anim.scale_value, 1.0);
        assert_eq!(anim.additive_opacity, 0.0);
        assert!(!anim.visual_changed_from(&settled));
    }

    #[test]
    fn hand_hover_uses_stable_pink_state() {
        let mut anim = CursorAnim::default();
        anim.pointer_hover = true;
        for _ in 0..120 {
            anim.update(1.0 / 60.0, 0.0, 0.0);
        }
        assert_eq!(anim.angle, super::POINTER_ANGLE);
        assert_eq!(anim.additive_opacity, 1.0);

        let settled = anim;
        anim.update(1.0 / 60.0, 0.0, 0.0);
        assert!(!anim.visual_changed_from(&settled));
    }

    #[test]
    fn resize_angle_overrides_hover_and_clears_on_press() {
        let mut anim = CursorAnim::default();
        anim.pointer_hover = true;
        anim.resize_angle = Some(180.0);
        for _ in 0..120 {
            anim.update(1.0 / 60.0, 0.0, 0.0);
        }
        assert!((anim.angle - 180.0).abs() < 0.1, "边框角度未收敛: {}", anim.angle);

        anim.begin_press();
        assert_eq!(anim.resize_angle, None);
        anim.update(1.0 / 60.0, 0.0, 0.0);
        assert!(anim.angle < 180.0, "按下后未回到默认旋转路径");
    }

    #[test]
    fn press_drag_and_release_follow_original_sequence() {
        let mut anim = CursorAnim::default();
        anim.begin_press();
        anim.update(1.0 / 60.0, 0.0, 0.0);
        assert!(anim.mouse_down);
        assert!(anim.additive_opacity > 0.0);
        assert!(anim.scale_value < 1.0);

        anim.drag_active = true;
        for _ in 0..30 {
            anim.update(1.0 / 60.0, 40.0, 20.0);
        }
        assert!(anim.angle.abs() > 0.5);

        anim.end_press();
        assert!(anim.spin_returning);
        for _ in 0..120 {
            anim.update(1.0 / 60.0, 40.0, 20.0);
        }
        assert!(!anim.mouse_down);
        assert!(!anim.spin_returning);
        assert!(!anim.elastic_returning);
        assert_eq!(anim.angle, 0.0);
        assert_eq!(anim.scale_value, 1.0);
        assert_eq!(anim.additive_opacity, 0.0);
    }

    #[test]
    fn drag_angle_accumulates_across_atan2_branch() {
        let mut anim = CursorAnim::default();
        anim.begin_press();
        anim.drag_active = true;

        // 顺着按下点周围转一圈。每个方向保持若干帧，模拟真实拖动
        // 时动画追随目标角度，而不是测试瞬间跳转。
        for (dx, dy) in [
            (0.0, -100.0),
            (100.0, 0.0),
            (0.0, 100.0),
            (-100.0, 0.0),
            (0.0, -100.0),
        ] {
            for _ in 0..30 {
                anim.update(1.0 / 60.0, dx, dy);
            }
        }

        // 如果把 atan2 的主值直接当作目标，最后会回到约 -155.7°；
        // 连续展开后应保留完整一圈，角度超过 180°。
        assert!(anim.angle > 180.0, "累计角度未跨过一圈: {}", anim.angle);

        anim.end_press();
        assert!(anim.spin_returning);
    }

    #[test]
    fn elastic_release_returns_promptly_for_large_angle() {
        let mut anim = CursorAnim::default();
        anim.begin_press();
        anim.drag_active = true;
        anim.angle = 360.0;
        anim.end_press();

        // 360° 正转 3 圈到 1440°，归一化后 = 0°，无需 elastic，动画就地结束。
        let mut elapsed = 0.0;
        while (anim.spin_returning || anim.elastic_returning) && elapsed < 3.0 {
            anim.update(1.0 / 60.0, 0.0, 0.0);
            elapsed += 1.0 / 60.0;
        }

        assert!(!anim.spin_returning);
        assert!(!anim.elastic_returning);
        // 0.5s spin + 从 ~1440° 衰减的 elastic，总时长应远小于 2.5s。
        assert!(elapsed < 2.5, "大角度释放仍然过慢: {elapsed:.3}s");
        assert_eq!(anim.angle, 0.0);
    }

    #[test]
    fn release_spins_three_turns_then_settles() {
        let mut anim = CursorAnim::default();
        anim.begin_press();
        anim.drag_active = true;
        anim.angle = 200.0;
        anim.end_press();
        assert!(anim.spin_returning);
        assert!(!anim.elastic_returning);

        // spin 阶段：推进到 spin 结束（约 0.5s）。结束帧角度归一化为 -160°
        // （200° 正转 3 圈到 1280°，normalize 后 = -160°，视觉连续无跳变）。
        let mut frames = 0;
        while anim.spin_returning && frames < 120 {
            anim.update(1.0 / 60.0, 0.0, 0.0);
            frames += 1;
        }
        assert!(!anim.spin_returning, "spin 未在 ~0.5s 结束");
        assert!(anim.elastic_returning, "spin 结束未衔接 elastic");
        assert!(
            (anim.angle - (-160.0)).abs() < 0.5,
            "spin 结束角度={}（期望 -160）",
            anim.angle
        );

        // elastic 阶段：跑满直到回归初始状态。
        let mut elapsed = 0.0;
        while anim.elastic_returning && elapsed < 3.0 {
            anim.update(1.0 / 60.0, 0.0, 0.0);
            elapsed += 1.0 / 60.0;
        }
        assert!(!anim.elastic_returning);
        assert_eq!(anim.angle, 0.0);
        assert_eq!(anim.scale_value, 1.0);
        assert_eq!(anim.additive_opacity, 0.0);
    }

    /// 原版 elastic_out 必须从 0 开始（否则释放瞬间角度跳变），
    /// 且回弹允许小幅越过终点。
    #[test]
    fn elastic_out_starts_at_zero() {
        let v0 = super::elastic_out(0.0);
        assert!(v0.abs() < 0.01, "elastic_out(0)={v0}");
        let mut max_value = 0.0f64;
        let mut t = 0.0f64;
        while t <= 1.0 {
            let v = super::elastic_out(t);
            if v > max_value {
                max_value = v;
            }
            t += 0.001;
        }
        println!("elastic_out max={max_value:.3}");
        assert!(
            max_value > 1.0,
            "elastic_out 未产生原版回弹过冲: max={max_value}"
        );
    }
}
