// JSON 编辑器 —— 类 JSON 文本的查看 / 格式化 / 压缩 / 过滤。
//
// 界面由 gpui-shell 在 GPUI 主线程渲染；解析与过滤是纯 JS（见 ./parsers.js、
// ./filter.js）。置顶由宿主按 extension.json 的 window.always_on_top 决定，这里的
// 图钉只是状态指示。
import { View, div, svg } from "gpui-kit";
import {
	Textarea,
	TextareaState,
	TextView,
	Input,
	InputState,
	Button,
	h_flex,
	v_flex,
} from "gpui-base";
import { readFile } from "fs/promises";
import {
	parseAny,
	pretty,
	minify,
	escapeText,
	unescapeText,
	summary,
} from "./parsers.js";
import { applyFilter } from "./filter.js";

// 一次「批量插入」至少这么多字符就当作粘贴处理（change 事件不带粘贴语义）。
const PASTE_MIN = 8;

// 右下角工具按钮：id、内置 Lucide 图标名、说明。
const TOOLS = [
	{ id: "fmt", icon: "braces", label: "重新格式化" },
	{ id: "min", icon: "shrink", label: "压缩（去掉空白）" },
	{ id: "min-copy", icon: "clipboard-copy", label: "压缩并复制" },
	{ id: "copy", icon: "copy", label: "复制原文" },
	{ id: "esc", icon: "quote", label: "转义为字符串" },
	{ id: "unesc", icon: "code", label: "反转义并格式化" },
	{ id: "paste-in", icon: "clipboard", label: "从剪贴板导入" },
	{ id: "clear", icon: "trash", label: "清空" },
	{ id: "about", icon: "info", label: "关于与支持的格式" },
];

export default class JsonEditor extends View {
	init(_props, cx) {
		this.editor = TextareaState.new({
			placeholder:
				"粘贴 JSON / JSONC / JS 对象 / URL 参数 / XML / YAML …",
			rows: 16,
		});
		this.filterInput = InputState.new({
			placeholder: "过滤：.key.subkey · [0][1] · .map(x=>x.val)",
		});
		this.lastText = "";
		this.suppress = false;
		this.status = "";
		this.formatName = "";
		this.resultValue = null;
		this.resultError = "";
		this.pinned = false;

		this.editor.on("change", (_event, cx) => this.onEditorChange(cx));
		this.filterInput.on("change", (_event, cx) => this.refreshFilter(cx));
		this.detectPinned(cx);
	}

	/// 图钉只是状态：真正的置顶在开窗时由清单决定，运行时改不了窗口层级。
	detectPinned(cx) {
		cx.spawn(async (taskCx) => {
			try {
				const raw = await readFile("extension.json", "utf8");
				const manifest = JSON.parse(raw);
				this.pinned = !!(
					manifest &&
					manifest.window &&
					manifest.window.always_on_top
				);
			} catch (_) {
				this.pinned = false;
			}
			taskCx.notify();
		});
	}

	// ── 编辑器事件 ──────────────────────────────────────────────────────────

	onEditorChange(cx) {
		if (this.suppress) return;
		const text = this.editor.value();
		if (text === this.lastText) return;
		const inserted = text.length - this.lastText.length;
		this.lastText = text;
		if (inserted >= PASTE_MIN) this.autoFormat(text, cx);
		else this.refreshFilter(cx);
		cx.notify();
	}

	/// 写回编辑器：抑制随后的 change 回调，避免自我格式化循环。
	writeEditor(text) {
		this.suppress = true;
		this.editor.set_value(text);
		this.lastText = text;
		this.suppress = false;
	}

	/// 粘贴进来时自动识别 + 美化。
	autoFormat(text, cx) {
		const parsed = parseAny(text);
		if (!parsed.ok) {
			this.status = "未识别：" + parsed.error;
			this.refreshFilter(cx);
			return;
		}
		this.formatName = parsed.format;
		this.writeEditor(pretty(parsed.value));
		this.status = "已识别为 " + parsed.format;
		this.refreshFilterWith(parsed.value);
	}

	// ── 过滤 ────────────────────────────────────────────────────────────────

	refreshFilter(_cx) {
		const parsed = parseAny(this.editor.value());
		if (!parsed.ok) {
			this.resultValue = null;
			this.resultError = parsed.error === "输入为空" ? "" : parsed.error;
			if (this.resultError === "") this.formatName = "";
			return;
		}
		this.formatName = parsed.format;
		this.refreshFilterWith(parsed.value);
	}

	refreshFilterWith(value) {
		const result = applyFilter(value, this.filterInput.value());
		if (!result.ok) {
			this.resultValue = null;
			this.resultError = result.error;
		} else if (result.inactive) {
			this.resultValue = null;
			this.resultError = "";
		} else {
			this.resultValue = result.value;
			this.resultError = "";
		}
	}

	resultMarkdown() {
		if (this.resultError) return "**过滤失败**\n\n" + this.resultError;
		return "```json\n" + pretty(this.resultValue) + "\n```";
	}

	// ── 右下角工具 ──────────────────────────────────────────────────────────

	runTool(id, cx) {
		const flash = (title) =>
			window.push_toast({ title, level: "info", id: "json-editor" });

		if (id === "clear") {
			this.writeEditor("");
			this.resultValue = null;
			this.resultError = "";
			this.status = "";
			this.formatName = "";
			cx.notify();
			return;
		}
		if (id === "about") {
			flash(
				"支持：JSON / JSONC / JSON5 / JS 对象字面量 / URL 参数 / XML / YAML",
			);
			return;
		}
		if (id === "paste-in") {
			const clip = cx.read_from_clipboard();
			if (clip === null || clip === undefined || clip === "") {
				this.status = "剪贴板为空";
				cx.notify();
				return;
			}
			const text = String(clip);
			this.writeEditor(text);
			this.autoFormat(text, cx);
			cx.notify();
			return;
		}

		const text = this.editor.value();
		const parsed = parseAny(text);
		if (!parsed.ok) {
			this.status = "未识别：" + parsed.error;
			cx.notify();
			return;
		}
		this.formatName = parsed.format;

		switch (id) {
			case "fmt":
				this.writeEditor(pretty(parsed.value));
				this.status = "已格式化";
				break;
			case "min":
				this.writeEditor(minify(parsed.value));
				this.status = "已压缩";
				break;
			case "min-copy":
				cx.write_to_clipboard(minify(parsed.value));
				flash("已复制压缩后的 JSON");
				break;
			case "copy":
				cx.write_to_clipboard(text);
				flash("已复制原文");
				break;
			case "esc":
				this.writeEditor(escapeText(text));
				this.status = "已转义为字符串";
				break;
			case "unesc": {
				const un = unescapeText(text);
				if (!un.ok) {
					this.status = un.error;
					break;
				}
				const again = parseAny(un.value);
				this.writeEditor(again.ok ? pretty(again.value) : un.value);
				this.status = again.ok
					? "已反转义（" + again.format + "）"
					: "已反转义";
				break;
			}
			default:
				break;
		}
		this.refreshFilter(cx);
		cx.notify();
	}

	// ── 视图 ────────────────────────────────────────────────────────────────

	render(cx) {
		const colors = cx.theme().colors;
		const showResult = this.resultValue !== null || this.resultError !== "";
		return v_flex()
			.size_full()
			.bg(colors.background)
			.child(this.header(colors))
			.child(
				Textarea.new(this.editor)
					.flex_1()
					.p(12)
					.text_size(13)
					.text_color(colors.foreground)
					.bg(colors.background),
			)
			.when(showResult, (el) => el.child(this.resultPanel(colors)))
			.child(this.bottomBar(colors, cx));
	}

	header(colors) {
		return h_flex()
			.items_center()
			.gap(8)
			.px(12)
			.py(8)
			.flex_shrink(0)
			.border_b(1)
			.border_color(colors.border)
			.child(
				svg("icons/braces.svg")
					.w(18)
					.h(18)
					.text_color(colors.foreground),
			)
			.child(
				div()
					.text_size(14)
					.font_semibold()
					.text_color(colors.foreground)
					.child("JSON 编辑器"),
			)
			.when(this.formatName !== "", (el) =>
				el.child(
					div()
						.text_size(11)
						.px(6)
						.py(1)
						.rounded(4)
						.bg(colors.muted)
						.text_color(colors.muted_foreground)
						.child(this.formatName),
				),
			)
			.child(div().flex_1())
			.child(this.pinBadge(colors));
	}

	pinBadge(colors) {
		const label = this.pinned
			? "已置顶：窗口层级由 extension.json 的 window.always_on_top 决定"
			: "未置顶：窗口层级由 extension.json 的 window.always_on_top 决定";
		return div()
			.flex()
			.items_center()
			.justify_center()
			.w(28)
			.h(28)
			.rounded(6)
			.tooltip(label)
			.accessibility_label(label)
			.child(
				svg(this.pinned ? "icons/pin.svg" : "icons/pin-off.svg")
					.w(16)
					.h(16)
					.text_color(
						this.pinned ? colors.accent : colors.muted_foreground,
					),
			);
	}

	resultPanel(colors) {
		return v_flex()
			.h(220)
			.flex_shrink(0)
			.border_t(1)
			.border_color(colors.border)
			.bg(colors.surface)
			.child(
				h_flex()
					.items_center()
					.gap(8)
					.px(12)
					.py(6)
					.flex_shrink(0)
					.child(
						div()
							.text_size(11)
							.font_semibold()
							.text_color(colors.muted_foreground)
							.child("过滤结果"),
					)
					.child(div().flex_1())
					.child(
						div()
							.text_size(11)
							.text_color(colors.muted_foreground)
							.child(
								this.resultError
									? "失败"
									: summary(this.resultValue),
							),
					),
			)
			.child(
				div()
					.flex_1()
					.overflow_y_scroll()
					.p(12)
					.child(
						TextView.markdown(
							"json-filter-result",
							this.resultMarkdown(),
						),
					),
			);
	}

	bottomBar(colors, cx) {
		return h_flex()
			.items_center()
			.gap(6)
			.px(12)
			.py(8)
			.flex_shrink(0)
			.border_t(1)
			.border_color(colors.border)
			.child(
				Input.new(this.filterInput)
					.flex_1()
					.h(28)
					.px(8)
					.border(1)
					.border_color(colors.input)
					.bg(colors.surface)
					.text_size(12),
			)
			.when(this.status !== "", (el) =>
				el.child(
					div()
						.text_size(11)
						.text_color(colors.muted_foreground)
						.child(this.status),
				),
			)
			.children(TOOLS.map((tool) => this.toolButton(tool, colors, cx)));
	}

	toolButton(tool, colors, cx) {
		return Button.new(tool.id)
			.flex()
			.items_center()
			.justify_center()
			.w(30)
			.h(28)
			.rounded(6)
			.bg(colors.surface)
			.border(1)
			.border_color(colors.border)
			.hover((style) => style.bg(colors.muted))
			.tooltip(tool.label)
			.accessibility_label(tool.label)
			.on_click((_event, cx) => {
				this.runTool(tool.id, cx);
				cx.notify();
			})
			.child(
				svg("icons/" + tool.icon + ".svg")
					.w(15)
					.h(15)
					.text_color(colors.foreground),
			);
	}
}
