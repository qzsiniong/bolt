// 「像 JSON 的字符串」解析器。
//
// 纯函数，不依赖任何宿主 API，可以直接用 node 自测（见 README）。
// 识别顺序：严格 JSON → 转义字符串解包 → 宽松 JSON / JS 对象字面量 → URL 参数
// → XML → YAML 子集。第一个成功的即返回；`format` 说明命中的是哪一种。

const LOOSE_FAIL = Symbol("loose-fail");

/** 把一段文本解析成值。返回 `{ ok, value, format }` 或 `{ ok: false, error }`。 */
export function parseAny(text) {
	if (typeof text !== "string" || text.trim() === "") {
		return { ok: false, error: "输入为空" };
	}
	const trimmed = text.trim();

	// 1. 严格 JSON。是字符串时再尝试解包一层（`"{\"a\":1}"`）。
	try {
		const value = JSON.parse(trimmed);
		if (typeof value === "string") {
			const inner = parseInner(value);
			if (inner) return { ok: true, value: inner, format: "转义字符串" };
		}
		return { ok: true, value, format: "JSON" };
	} catch (_) {
		// 继续尝试其它格式。
	}

	// 2. 宽松 JSON / JS 对象字面量（注释、尾逗号、单引号、裸键、undefined…）。
	const loose = looseParse(trimmed);
	if (loose !== LOOSE_FAIL) {
		return { ok: true, value: loose, format: "宽松 JSON / JS 对象" };
	}

	// 3. URL 参数。
	const query = parseQuery(trimmed);
	if (query) return { ok: true, value: query, format: "URL 参数" };

	// 4. XML。
	const xml = parseXml(trimmed);
	if (xml) return { ok: true, value: xml, format: "XML" };

	// 5. YAML 子集。
	const yaml = parseYaml(trimmed);
	if (yaml !== null) return { ok: true, value: yaml, format: "YAML" };

	return {
		ok: false,
		error: "无法识别为 JSON / JS 对象 / URL 参数 / XML / YAML",
	};
}

/** 供「转义字符串解包」使用：先严格 JSON，再宽松解析。 */
function parseInner(text) {
	const trimmed = text.trim();
	if (trimmed === "") return null;
	try {
		return JSON.parse(trimmed);
	} catch (_) {
		const loose = looseParse(trimmed);
		return loose === LOOSE_FAIL ? null : loose;
	}
}

// ── 宽松 JSON / JS 对象字面量 ────────────────────────────────────────────────

class LooseError extends Error {}

function looseParse(input) {
	const s = input;
	let i = 0;

	function fail(message) {
		throw new LooseError(message || "解析失败");
	}

	function skip() {
		for (;;) {
			while (i < s.length && /\s/.test(s[i])) i += 1;
			if (s[i] === "/" && s[i + 1] === "/") {
				while (i < s.length && s[i] !== "\n") i += 1;
				continue;
			}
			if (s[i] === "/" && s[i + 1] === "*") {
				i += 2;
				while (i < s.length && !(s[i] === "*" && s[i + 1] === "/"))
					i += 1;
				if (i >= s.length) fail("块注释未闭合");
				i += 2;
				continue;
			}
			break;
		}
	}

	function readString(quote) {
		i += 1;
		let out = "";
		while (i < s.length) {
			const c = s[i];
			if (c === "\\") {
				const n = s[i + 1];
				if (n === undefined) fail("字符串未闭合");
				if (n === "u") {
					const hex = s.slice(i + 2, i + 6);
					if (/^[0-9a-fA-F]{4}$/.test(hex)) {
						out += String.fromCharCode(parseInt(hex, 16));
						i += 6;
						continue;
					}
					out += "u";
				} else if (n === "n") out += "\n";
				else if (n === "t") out += "\t";
				else if (n === "r") out += "\r";
				else if (n === "b") out += "\b";
				else if (n === "f") out += "\f";
				else if (n === "v") out += "\v";
				else if (n === "0") out += "\0";
				else out += n;
				i += 2;
				continue;
			}
			if (c === quote) {
				i += 1;
				return out;
			}
			if (c === "\n") fail("字符串未闭合");
			out += c;
			i += 1;
		}
		fail("字符串未闭合");
	}

	function readNumber() {
		const rest = s.slice(i);
		let m = /^0[xX][0-9a-fA-F_]+/.exec(rest);
		if (m) {
			i += m[0].length;
			skipNumericSuffix();
			return parseInt(m[0].replace(/_/g, "").slice(2), 16);
		}
		m = /^0[bB][01_]+/.exec(rest);
		if (m) {
			i += m[0].length;
			skipNumericSuffix();
			return parseInt(m[0].replace(/_/g, "").slice(2), 2);
		}
		m = /^0[oO][0-7_]+/.exec(rest);
		if (m) {
			i += m[0].length;
			skipNumericSuffix();
			return parseInt(m[0].replace(/_/g, "").slice(2), 8);
		}
		m = /^[+-]?(Infinity|NaN)/.exec(rest);
		if (m) {
			i += m[0].length;
			if (m[0].indexOf("NaN") >= 0) return NaN;
			return m[0][0] === "-" ? -Infinity : Infinity;
		}
		m = /^[+-]?(\d[\d_]*\.?[\d_]*|\.\d[\d_]*)([eE][+-]?\d+)?/.exec(rest);
		if (!m) fail("非法数字");
		i += m[0].length;
		skipNumericSuffix();
		return Number(m[0].replace(/_/g, ""));
	}

	function skipNumericSuffix() {
		if (s[i] === "n") i += 1;
	}

	function readIdentifier() {
		if (i >= s.length || !/[A-Za-z_$]/.test(s[i])) return null;
		const start = i;
		while (i < s.length && /[A-Za-z0-9_$]/.test(s[i])) i += 1;
		return s.slice(start, i);
	}

	function readValue() {
		skip();
		if (i >= s.length) fail("意外的结尾");
		const c = s[i];
		if (c === "{") return readObject();
		if (c === "[") return readArray();
		if (c === '"' || c === "'") return readString(c);
		if (c === "-" || c === "+" || c === "." || (c >= "0" && c <= "9")) {
			return readNumber();
		}
		const ident = readIdentifier();
		if (ident !== null) {
			if (ident === "true") return true;
			if (ident === "false") return false;
			if (ident === "null") return null;
			if (ident === "undefined") return null;
			if (ident === "NaN") return null;
			if (ident === "Infinity") return Infinity;
			fail("未知标识符 " + ident);
		}
		fail("无法解析字符 " + JSON.stringify(c));
	}

	function readObject() {
		i += 1;
		const obj = {};
		skip();
		if (s[i] === "}") {
			i += 1;
			return obj;
		}
		for (;;) {
			skip();
			let key;
			const c = s[i];
			if (c === '"' || c === "'") key = readString(c);
			else {
				const id = readIdentifier();
				if (id === null) fail("期望键名");
				key = id;
			}
			skip();
			if (s[i] !== ":") fail("期望 :");
			i += 1;
			obj[key] = readValue();
			skip();
			if (s[i] === ",") {
				i += 1;
				skip();
				if (s[i] === "}") {
					i += 1;
					return obj;
				}
				continue;
			}
			if (s[i] === "}") {
				i += 1;
				return obj;
			}
			fail("期望 , 或 }");
		}
	}

	function readArray() {
		i += 1;
		const arr = [];
		skip();
		if (s[i] === "]") {
			i += 1;
			return arr;
		}
		for (;;) {
			arr.push(readValue());
			skip();
			if (s[i] === ",") {
				i += 1;
				skip();
				if (s[i] === "]") {
					i += 1;
					return arr;
				}
				continue;
			}
			if (s[i] === "]") {
				i += 1;
				return arr;
			}
			fail("期望 , 或 ]");
		}
	}

	try {
		const value = readValue();
		skip();
		// 允许尾随的 `as const`（含 `satisfies` 等）被忽略。
		if (s.slice(i).trim().indexOf("as ") === 0) i = s.length;
		skip();
		if (i !== s.length) return LOOSE_FAIL;
		return value;
	} catch (e) {
		return LOOSE_FAIL;
	}
}

// ── URL 参数 ────────────────────────────────────────────────────────────────

function parseQuery(text) {
	if (text.indexOf("=") < 0) return null;
	if (/[{}<>]/.test(text)) return null;
	const raw = text.charAt(0) === "?" ? text.slice(1) : text;
	const parts = raw.split("&").filter((part) => part !== "");
	if (parts.length === 0) return null;
	for (const part of parts) if (part.indexOf("=") < 0) return null;

	const obj = {};
	for (const part of parts) {
		const eq = part.indexOf("=");
		const key = decode(part.slice(0, eq));
		const value = decode(part.slice(eq + 1));
		const m = /^([^\[]+)\[\]$/.exec(key);
		if (m) {
			const name = m[1];
			if (!Array.isArray(obj[name])) {
				obj[name] = obj[name] === undefined ? [] : [obj[name]];
			}
			obj[name].push(value);
		} else if (obj[key] === undefined) {
			obj[key] = value;
		} else if (Array.isArray(obj[key])) {
			obj[key].push(value);
		} else {
			obj[key] = [obj[key], value];
		}
	}
	return obj;
}

function decode(value) {
	try {
		return decodeURIComponent(value.replace(/\+/g, " "));
	} catch (_) {
		return value;
	}
}

// ── XML ─────────────────────────────────────────────────────────────────────

function parseXml(text) {
	if (text.charAt(0) !== "<") return null;
	if (!/<\/[A-Za-z_]/.test(text)) return null;
	const tokens = text.match(
		/<!--[\s\S]*?-->|<\?[\s\S]*?\?>|<!\[CDATA\[[\s\S]*?\]\]>|<\/[^>]*>|<[^>]*>|[^<]+/g,
	);
	if (!tokens) return null;

	const root = { name: "#root", attrs: {}, children: [], text: "" };
	const stack = [root];
	for (const token of tokens) {
		if (token.indexOf("<!--") === 0 || token.indexOf("<?") === 0) continue;
		if (token.indexOf("<![CDATA[") === 0) {
			stack[stack.length - 1].text += token.slice(9, -3);
			continue;
		}
		if (token.indexOf("</") === 0) {
			if (stack.length <= 1) return null;
			const node = stack.pop();
			stack[stack.length - 1].children.push(node);
			continue;
		}
		if (token.charAt(0) === "<") {
			const selfClosing = /\/>$/.test(token);
			const body = token.slice(1, selfClosing ? -2 : -1).trim();
			const m = /^([A-Za-z_][\w:.-]*)/.exec(body);
			if (!m) return null;
			const attrs = {};
			const re = /([A-Za-z_][\w:.-]*)\s*=\s*("([^"]*)"|'([^']*)')/g;
			const rest = body.slice(m[1].length);
			let mm;
			while ((mm = re.exec(rest))) {
				attrs[mm[1]] = mm[3] !== undefined ? mm[3] : mm[4];
			}
			const node = { name: m[1], attrs, children: [], text: "" };
			if (selfClosing) stack[stack.length - 1].children.push(node);
			else stack.push(node);
			continue;
		}
		const textPart = token.trim();
		if (textPart) stack[stack.length - 1].text += textPart;
	}
	if (stack.length !== 1) return null;
	return groupXmlChildren(root);
}

function groupXmlChildren(node) {
	const out = {};
	const grouped = {};
	for (const child of node.children) {
		if (!grouped[child.name]) grouped[child.name] = [];
		grouped[child.name].push(xmlNodeValue(child));
	}
	for (const name of Object.keys(grouped)) {
		out[name] =
			grouped[name].length === 1 ? grouped[name][0] : grouped[name];
	}
	return out;
}

function xmlNodeValue(node) {
	const attrKeys = Object.keys(node.attrs);
	const out = {};
	for (const key of attrKeys) out["@" + key] = node.attrs[key];
	if (node.children.length === 0 && attrKeys.length === 0) return node.text;
	if (node.children.length > 0) {
		const grouped = {};
		for (const child of node.children) {
			if (!grouped[child.name]) grouped[child.name] = [];
			grouped[child.name].push(xmlNodeValue(child));
		}
		for (const name of Object.keys(grouped)) {
			out[name] =
				grouped[name].length === 1 ? grouped[name][0] : grouped[name];
		}
	}
	if (node.text !== "") out["#text"] = node.text;
	return out;
}

// ── YAML 子集 ───────────────────────────────────────────────────────────────

function parseYaml(text) {
	const lines = [];
	for (const raw of text.split(/\r?\n/)) {
		const stripped = stripYamlComment(raw);
		if (stripped.trim() === "") continue;
		const content = stripped.replace(/\s+$/, "");
		const indent = content.length - content.replace(/^[\s]+/, "").length;
		lines.push({ indent, text: content.trim() });
	}
	if (lines.length === 0) return null;
	if (!looksLikeYaml(lines[0].text)) return null;
	const block = parseYamlBlock(lines, 0, lines[0].indent);
	if (!block || block.next !== lines.length) return null;
	return block.value;
}

function looksLikeYaml(first) {
	if (/^-(\s|$)/.test(first)) return true;
	return /^["']?[^:]+:(\s|$)/.test(first);
}

function isSeqItem(text) {
	return /^-(\s|$)/.test(text);
}

function parseYamlBlock(lines, start, indent) {
	if (start >= lines.length) return null;
	return isSeqItem(lines[start].text)
		? parseYamlSequence(lines, start, indent)
		: parseYamlMapping(lines, start, indent);
}

function parseYamlMapping(lines, start, indent) {
	const obj = {};
	let i = start;
	while (
		i < lines.length &&
		lines[i].indent === indent &&
		!isSeqItem(lines[i].text)
	) {
		const m = /^([^:]+):(\s*)(.*)$/.exec(lines[i].text);
		if (!m) return null;
		const key = unquote(m[1].trim());
		const rest = m[3].trim();
		if (rest === "") {
			if (i + 1 < lines.length && lines[i + 1].indent > indent) {
				const child = parseYamlBlock(lines, i + 1, lines[i + 1].indent);
				if (!child) return null;
				obj[key] = child.value;
				i = child.next;
			} else {
				obj[key] = null;
				i += 1;
			}
		} else if (/^[|>][-+]?$/.test(rest)) {
			const scalar = readYamlBlockScalar(lines, i + 1, indent);
			obj[key] = scalar.value;
			i = scalar.next;
		} else {
			obj[key] = parseYamlScalar(rest);
			i += 1;
		}
	}
	return { value: obj, next: i };
}

function parseYamlSequence(lines, start, indent) {
	const arr = [];
	let i = start;
	while (
		i < lines.length &&
		lines[i].indent === indent &&
		isSeqItem(lines[i].text)
	) {
		const content = lines[i].text.replace(/^-\s*/, "");
		if (content === "") {
			if (i + 1 < lines.length && lines[i + 1].indent > indent) {
				const child = parseYamlBlock(lines, i + 1, lines[i + 1].indent);
				if (!child) return null;
				arr.push(child.value);
				i = child.next;
			} else {
				arr.push(null);
				i += 1;
			}
		} else if (/^[^:]+:(\s|$)/.test(content)) {
			// `- key: value`：把本行与后续更深缩进的行合起来按映射解析。
			const sub = [{ indent: indent + 2, text: content }];
			let j = i + 1;
			while (j < lines.length && lines[j].indent > indent) {
				sub.push(lines[j]);
				j += 1;
			}
			const child = parseYamlMapping(sub, 0, indent + 2);
			if (!child || child.next !== sub.length) return null;
			arr.push(child.value);
			i = j;
		} else {
			arr.push(parseYamlScalar(content));
			i += 1;
		}
	}
	return { value: arr, next: i };
}

function readYamlBlockScalar(lines, start, parentIndent) {
	const parts = [];
	let i = start;
	while (i < lines.length && lines[i].indent > parentIndent) {
		parts.push(lines[i].text);
		i += 1;
	}
	return { value: parts.join("\n"), next: i };
}

function parseYamlScalar(text) {
	const s = text.trim();
	if (s === "" || s === "~" || s === "null" || s === "Null" || s === "NULL")
		return null;
	if (s === "true" || s === "True" || s === "TRUE") return true;
	if (s === "false" || s === "False" || s === "FALSE") return false;
	if (/^[+-]?\d+$/.test(s)) return Number(s);
	if (/^[+-]?\d*\.\d+$/.test(s)) return Number(s);
	if (s.charAt(0) === "[" || s.charAt(0) === "{") {
		const value = looseParse(s);
		if (value !== LOOSE_FAIL) return value;
	}
	return unquote(s);
}

function stripYamlComment(line) {
	let inSingle = false;
	let inDouble = false;
	for (let i = 0; i < line.length; i += 1) {
		const c = line[i];
		if (c === "'" && !inDouble) inSingle = !inSingle;
		else if (c === '"' && !inSingle) inDouble = !inDouble;
		else if (
			c === "#" &&
			!inSingle &&
			!inDouble &&
			(i === 0 || line[i - 1] === " " || line[i - 1] === "\t")
		) {
			return line.slice(0, i);
		}
	}
	return line;
}

function unquote(text) {
	if (text.length >= 2) {
		const a = text.charAt(0);
		const b = text.charAt(text.length - 1);
		if ((a === '"' && b === '"') || (a === "'" && b === "'"))
			return text.slice(1, -1);
	}
	return text;
}

// ── 输出格式化 ──────────────────────────────────────────────────────────────

/** 美化：两空格缩进。 */
export function pretty(value) {
	return JSON.stringify(value, null, 2);
}

/** 压缩：去掉所有多余空白。 */
export function minify(value) {
	return JSON.stringify(value);
}

/** 把整段文本转义成一个 JSON 字符串字面量。 */
export function escapeText(text) {
	return JSON.stringify(text);
}

/** 反转义一个被引号包裹的字符串字面量。 */
export function unescapeText(text) {
	const trimmed = text.trim();
	const first = trimmed.charAt(0);
	const last = trimmed.charAt(trimmed.length - 1);
	if (!((first === '"' && last === '"') || (first === "'" && last === "'"))) {
		return { ok: false, error: "不是被引号包裹的字符串" };
	}
	try {
		const value = JSON.parse(trimmed);
		if (typeof value !== "string")
			return { ok: false, error: "解出来的不是字符串" };
		return { ok: true, value };
	} catch (e) {
		return { ok: false, error: "转义解析失败：" + e.message };
	}
}

/** 一行摘要，用于结果面板标题。 */
export function summary(value) {
	if (Array.isArray(value)) return "array · " + value.length + " 项";
	if (value === null) return "null";
	const type = typeof value;
	if (type === "object")
		return "object · " + Object.keys(value).length + " 键";
	if (type === "string") return "string · " + value.length + " 字符";
	return type + " · " + String(value);
}
