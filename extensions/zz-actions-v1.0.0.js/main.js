import { View, div } from "gpui-kit";
import { v_flex } from "gpui-base";

export default class ActionsDemo extends View {
	init(_props, cx) {}

	render(cx) {
		const colors = cx.theme().colors;
		return v_flex()
			.size_full()
			.bg(colors.background)
			.child("悬停标题栏按钮应出现 tooltip");
	}
}
