
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


export default class JsonEditor extends View {
	init(_props, cx) {

	}

	// ── 视图 ────────────────────────────────────────────────────────────────

	render(cx) {
		const colors = cx.theme().colors;
		return v_flex()
			.size_full()
			.bg(colors.background)
			.child("@@@hello world@");
	}
}
