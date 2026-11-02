// 演示扩展 —— UI 命令入口（入口固定为 main.js，不可改名）。
//
// 脚本只描述界面，渲染由宿主在 GPUI 主线程完成（没有 WebView）；
// debug 构建下保存本文件即热重载。窗口根是 gpui-shell 的 ShellRoot，
// 所以能用 shell 自带的浮层（push_toast）与剪贴板（capabilities.clipboard）。
// base 的控件都不带视觉样式，样式全部写在脚本里，颜色用主题语义令牌。
import { View, div, svg } from "gpui-kit";
import {
  h_flex,
  v_flex,
  Button,
  Input,
  InputState,
  Checkbox,
  Switch,
  Link,
  TextView,
} from "gpui-base";

export default class DemoPanel extends View {
  init() {
    // localStorage 由宿主固定授予（storage），落在扩展数据目录的 store.json。
    this.count = Number(localStorage.getItem("count") ?? 0);
    this.draft = InputState.new({ placeholder: "输入点什么，回车回显…" });
    this.draft.on("submit", (_event, cx) => this.echo(cx));
    this.agree = false;
    this.warp = false;
  }

  // ── 行为 ──────────────────────────────────────────────────────────────

  /** 输入框回车：弹一个 toast（shell 自带浮层）。 */
  echo(cx) {
    const text = this.draft.value().trim();
    if (text === "") return;
    window.push_toast({ title: `你输入了：${text}`, level: "info", id: "echo" });
    this.draft.set_value("");
    cx.notify();
  }

  /** 把当前计数写进剪贴板（ui.capabilities.clipboard 授权后可用）。 */
  copyCount(cx) {
    cx.write_to_clipboard(`来自演示面板：计数 ${this.count}`);
    window.push_toast({ title: "已复制到剪贴板", level: "success", id: "copy" });
  }

  /** 从剪贴板读回来填进输入框。 */
  pasteIn(cx) {
    const text = cx.read_from_clipboard();
    if (text === undefined || text === null || text === "") {
      window.push_toast({ title: "剪贴板里没有文本", level: "warning", id: "paste" });
      return;
    }
    this.draft.set_value(String(text));
    cx.notify();
  }

  /** 计数并持久化：重启应用后还在。 */
  bump(cx, delta) {
    this.count += delta;
    localStorage.setItem("count", String(this.count));
    cx.notify();
  }

  // ── 视图 ──────────────────────────────────────────────────────────────

  render(cx) {
    const colors = cx.theme().colors;
    return v_flex()
      .size_full()
      .p(24)
      .gap(14)
      .bg(colors.background)
      .child(this.header(colors))
      .child(this.counterRow(colors))
      .child(this.inputRow(colors))
      .child(this.toggles(colors))
      .child(
        TextView.markdown(
          "notes",
          "试试标题栏右侧的按钮：**内建动作**（重载 / 打开目录 / 关窗）与**脚本命令动作**（输出进 Console）。计数已通过 `localStorage` 存进扩展数据目录。",
        ).text_size(12),
      )
      .child(this.footer(colors));
  }

  header(colors) {
    return h_flex()
      .items_center()
      .gap(8)
      .flex_shrink(0)
      .child(
        svg("icons/star.svg").w(18).h(18).text_color(colors.foreground),
      )
      .child(
        div()
          .text_size(16)
          .font_semibold()
          .text_color(colors.foreground)
          .child("演示面板"),
      )
      .child(
        div()
          .text_size(12)
          .text_color(colors.muted_foreground)
          .child("扩展的全部能力，一个窗口看完"),
      );
  }

  counterRow(colors) {
    return h_flex()
      .gap(8)
      .items_center()
      .flex_shrink(0)
      // 注意：闭包参数里的 cx 是 on_click 回调当场给的，不是 render 时的——
      // 捕获 render 的 cx 留到点击时用会报「cx is no longer valid」。
      .child(this.button("dec", "−", colors, (cx) => this.bump(cx, -1)))
      .child(
        div()
          .min_w(56)
          .text_center()
          .text_size(13)
          .text_color(colors.foreground)
          .child(`计数：${this.count}`),
      )
      .child(this.button("inc", "＋", colors, (cx) => this.bump(cx, 1)))
      .child(div().flex_1())
      .child(this.button("copy", "复制计数", colors, (cx) => this.copyCount(cx)))
      .child(this.button("paste", "粘贴进输入框", colors, (cx) => this.pasteIn(cx)));
  }

  inputRow(colors) {
    return h_flex()
      .gap(8)
      .items_center()
      .flex_shrink(0)
      .child(
        Input.new(this.draft)
          .flex_1()
          .h(28)
          .px(8)
          .text_size(12)
          .border(1)
          .border_color(colors.input)
          .bg(colors.surface)
          .text_color(colors.foreground),
      )
      .child(this.button("echo", "回显", colors, (cx) => this.echo(cx)));
  }

  /** Checkbox 与 Switch 都是受控的：checked(...) 进，on_change(...) 出，指示器自己画。 */
  toggles(colors) {
    return v_flex()
      .gap(6)
      .flex_shrink(0)
      .child(
        Checkbox.new("agree")
          .checked(this.agree)
          .flex()
          .items_center()
          .gap(8)
          .on_change((checked, cx) => {
            this.agree = checked;
            window.push_toast({
              title: checked ? "已勾选" : "已取消勾选",
              level: "info",
              id: "agree",
            });
            cx.notify();
          })
          .child(
            div().text_size(12).text_color(colors.foreground).child("复选框（指示器是画出来的）"),
          ),
      )
      .child(
        Switch.new("warp")
          .checked(this.warp)
          .flex()
          .items_center()
          .gap(8)
          .on_change((checked, cx) => {
            this.warp = checked;
            cx.notify();
          })
          .child(
            div()
              .text_size(12)
              .text_color(colors.foreground)
              .child(`开关（当前 ${this.warp ? "开" : "关"}）`),
          ),
      );
  }

  footer(colors) {
    return h_flex()
      .items_center()
      .gap(4)
      .flex_shrink(0)
      .child(
        div().text_size(11).text_color(colors.muted_foreground).child("图标可用"),
      )
      .child(
        Link.new("lucide")
          .href("https://lucide.dev/icons")
          .child(
            div()
              .text_size(11)
              .text_color(colors.ring)
              .child("Lucide 图标集"),
          ),
      );
  }

  /**
   * base 的 Button 不带视觉样式，样式由脚本写。
   *
   * `run` 收到的 cx 来自 on_click 回调参数（每次点击都是新的）——闭包里不要
   * 捕获 render 时的 cx，那会在点击时报「cx is no longer valid」。
   */
  button(id, caption, colors, run) {
    return Button.new(id)
      .flex()
      .items_center()
      .justify_center()
      .h(28)
      .px(12)
      .bg(colors.surface)
      .text_color(colors.foreground)
      .text_size(12)
      .border(1)
      .border_color(colors.border)
      .rounded(6)
      .hover((style) => style.bg(colors.muted))
      .on_click((_event, cx) => {
        run(cx);
        cx.notify();
      })
      .child(caption);
  }
}
