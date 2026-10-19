// 「JS 过滤」表达式的受限求值器。
//
// 沙箱禁用了 `eval` 与所有 `Function` 构造器，所以这里手写一个只覆盖文档示例语法的
// 求值器：`.key.subkey`、`[0][1]`、`.map(x=>x.val)`、`.filter(x=>x.age>=18)` 等。
// 纯函数，不依赖宿主 API，可以用 node 自测（见 README）。

const METHODS = [
	"map",
	"filter",
	"find",
	"slice",
	"keys",
	"values",
	"sort",
	"reverse",
	"join",
	"flat",
];

class FilterError extends Error {}

/**
 * 对解析出来的值应用过滤表达式。
 *
 * 返回 `{ ok: true, value }`、`{ ok: true, value, inactive: true }`（表达式为空，
 * 原样返回）或 `{ ok: false, error }`（错误信息带字符位置）。
 */
export function applyFilter(value, expr) {
	const text = (
		expr === undefined || expr === null ? "" : String(expr)
	).trim();
	if (text === "") return { ok: true, value, inactive: true };

	let current = value;
	let i = 0;
	while (i < text.length) {
		const c = text.charAt(i);
		if (c !== "." && c !== "[") {
			return {
				ok: false,
				error:
					"无法识别的字符 " +
					JSON.stringify(c) +
					"（第 " +
					(i + 1) +
					" 个字符）",
			};
		}
		const segment =
			c === "." ? readDotSegment(text, i) : readIndexSegment(text, i);
		if (segment.error) {
			return {
				ok: false,
				error: segment.error + "（第 " + (i + 1) + " 个字符）",
			};
		}
		i = segment.next;
		const step = applySegment(segment, current);
		if (!step.ok) {
			return {
				ok: false,
				error: step.error + "（第 " + (segment.start + 1) + " 个字符）",
			};
		}
		current = step.value;
	}
	return { ok: true, value: current };
}

function readDotSegment(text, i) {
	const start = i;
	const m = /^[A-Za-z_$][\w$]*/.exec(text.slice(i + 1));
	if (!m) return { error: "期望属性名" };
	let j = i + 1 + m[0].length;
	if (text.charAt(j) === "(") {
		const args = readArgs(text, j);
		if (!args) return { error: "参数列表不完整" };
		return {
			kind: "call",
			name: m[0],
			args: args.list,
			start,
			next: args.next,
		};
	}
	return { kind: "prop", name: m[0], start, next: j };
}

function readIndexSegment(text, i) {
	const start = i;
	const close = text.indexOf("]", i + 1);
	if (close < 0) return { error: "缺少 ]" };
	const inner = text.slice(i + 1, close).trim();
	if (/^-?\d+$/.test(inner))
		return { kind: "index", index: Number(inner), start, next: close + 1 };
	const a = inner.charAt(0);
	const b = inner.charAt(inner.length - 1);
	if ((a === '"' && b === '"') || (a === "'" && b === "'")) {
		return {
			kind: "prop",
			name: inner.slice(1, -1),
			start,
			next: close + 1,
		};
	}
	return { error: "下标只能是数字或字符串" };
}

/** 从 `(` 开始读到匹配的 `)`，按顶层逗号切参数。 */
function readArgs(text, i) {
	let j = i + 1;
	let depth = 1;
	let buffer = "";
	const list = [];
	let quote = null;
	while (j < text.length) {
		const c = text.charAt(j);
		if (quote) {
			buffer += c;
			if (c === "\\") {
				buffer += text.charAt(j + 1);
				j += 2;
				continue;
			}
			if (c === quote) quote = null;
			j += 1;
			continue;
		}
		if (c === '"' || c === "'") {
			quote = c;
			buffer += c;
			j += 1;
			continue;
		}
		if (c === "(" || c === "[" || c === "{") {
			depth += 1;
			buffer += c;
			j += 1;
			continue;
		}
		if (c === ")" || c === "]" || c === "}") {
			depth -= 1;
			if (depth === 0) {
				if (buffer.trim() !== "") list.push(buffer.trim());
				return { list, next: j + 1 };
			}
			buffer += c;
			j += 1;
			continue;
		}
		if (c === "," && depth === 1) {
			list.push(buffer.trim());
			buffer = "";
			j += 1;
			continue;
		}
		buffer += c;
		j += 1;
	}
	return null;
}

function applySegment(segment, current) {
	if (segment.kind === "prop") return readProp(current, segment.name);
	if (segment.kind === "index") return readIndex(current, segment.index);
	return callMethod(segment.name, current, segment.args);
}

function readProp(current, name) {
	if (current === null || current === undefined) {
		return { ok: false, error: "在 null/undefined 上读取 " + name };
	}
	if (
		name === "length" &&
		(Array.isArray(current) || typeof current === "string")
	) {
		return { ok: true, value: current.length };
	}
	if (!(name in Object(current)))
		return { ok: false, error: "没有属性 " + name };
	return { ok: true, value: current[name] };
}

function readIndex(current, index) {
	if (Array.isArray(current) || typeof current === "string") {
		const at = index < 0 ? current.length + index : index;
		if (at < 0 || at >= current.length)
			return { ok: false, error: "下标 " + index + " 越界" };
		return { ok: true, value: current[at] };
	}
	if (current !== null && typeof current === "object") {
		const key = String(index);
		if (!(key in current)) return { ok: false, error: "没有属性 " + key };
		return { ok: true, value: current[key] };
	}
	return { ok: false, error: "无法下标取值" };
}

function callMethod(name, current, args) {
	if (METHODS.indexOf(name) < 0)
		return { ok: false, error: "不支持的方法 " + name };

	if (name === "map" || name === "filter" || name === "find") {
		if (!Array.isArray(current))
			return { ok: false, error: name + " 只能用于数组" };
		const lambda = compileLambda(args[0]);
		if (!lambda.ok) return lambda;
		try {
			// `map` 保留表达式求出的值；`filter`/`find` 按真假判定。
			if (name === "map")
				return {
					ok: true,
					value: current.map((item) => lambda.run(item)),
				};
			if (name === "filter")
				return {
					ok: true,
					value: current.filter((item) => truthy(lambda.run(item))),
				};
			const found = current.find((item) => truthy(lambda.run(item)));
			return { ok: true, value: found === undefined ? null : found };
		} catch (e) {
			return {
				ok: false,
				error: e instanceof FilterError ? e.message : "lambda 求值失败",
			};
		}
	}

	if (name === "slice") {
		if (!Array.isArray(current) && typeof current !== "string") {
			return { ok: false, error: "slice 只能用于数组或字符串" };
		}
		if (args.some((a) => !/^-?\d+$/.test(a)))
			return { ok: false, error: "slice 参数必须是整数" };
		const nums = args.map(Number);
		return { ok: true, value: current.slice(nums[0], nums[1]) };
	}

	if (name === "join") {
		if (!Array.isArray(current))
			return { ok: false, error: "join 只能用于数组" };
		let sep = ",";
		if (args.length > 0) {
			const lit = parseLiteral(args[0]);
			sep = lit.ok ? String(lit.value) : args[0];
		}
		return { ok: true, value: current.join(sep) };
	}

	if (name === "keys" || name === "values") {
		if (current === null || typeof current !== "object") {
			return { ok: false, error: name + " 只能用于对象" };
		}
		return {
			ok: true,
			value:
				name === "keys" ? Object.keys(current) : Object.values(current),
		};
	}

	if (name === "sort") {
		if (!Array.isArray(current))
			return { ok: false, error: "sort 只能用于数组" };
		return {
			ok: true,
			value: current.slice().sort((a, b) => (a > b ? 1 : a < b ? -1 : 0)),
		};
	}

	if (name === "reverse") {
		if (!Array.isArray(current))
			return { ok: false, error: "reverse 只能用于数组" };
		return { ok: true, value: current.slice().reverse() };
	}

	if (name === "flat") {
		if (!Array.isArray(current))
			return { ok: false, error: "flat 只能用于数组" };
		const depth = args.length > 0 ? Number(args[0]) : 1;
		if (!Number.isInteger(depth))
			return { ok: false, error: "flat 的参数必须是整数" };
		return { ok: true, value: current.flat(depth) };
	}

	return { ok: false, error: "不支持的方法 " + name };
}

/** 编译 `x => <表达式>`。表达式支持 `!`、路径链、比较运算符与字面量。 */
function compileLambda(source) {
	const m = /^\s*([A-Za-z_$][\w$]*)\s*=>\s*([\s\S]*)$/.exec(
		source === undefined ? "" : source,
	);
	if (!m) return { ok: false, error: "期望形如 x => x.field 的表达式" };
	const param = m[1];
	const body = m[2].trim();
	if (body === "") return { ok: false, error: "lambda 表达式为空" };

	let text = body;
	let negate = false;
	if (text.charAt(0) === "!") {
		negate = true;
		text = text.slice(1).trim();
	}

	const comparison = findComparison(text);
	let leftSource = text;
	let operator = null;
	let rightSource = null;
	if (comparison) {
		operator = comparison.op;
		leftSource = text.slice(0, comparison.at).trim();
		rightSource = text.slice(comparison.at + operator.length).trim();
	}

	const path = parsePath(leftSource, param);
	if (!path.ok) return path;

	let right = null;
	if (operator) {
		const lit = parseLiteral(rightSource);
		if (!lit.ok) return lit;
		right = lit.value;
	}

	return {
		ok: true,
		// 无比较运算符时保留路径求出的原始值（`map` 需要它）；有比较或前置 `!`
		// 时给出布尔。
		run: (item) => {
			const left = path.get(item);
			const result = operator ? compare(left, operator, right) : left;
			return negate ? !truthy(result) : result;
		},
	};
}

/** 找到顶层（不在括号/引号里）的比较运算符。 */
function findComparison(text) {
	let quote = null;
	let depth = 0;
	for (let i = 0; i < text.length; i += 1) {
		const c = text.charAt(i);
		if (quote) {
			if (c === "\\") {
				i += 1;
				continue;
			}
			if (c === quote) quote = null;
			continue;
		}
		if (c === '"' || c === "'") {
			quote = c;
			continue;
		}
		if (c === "[" || c === "(") {
			depth += 1;
			continue;
		}
		if (c === "]" || c === ")") {
			depth -= 1;
			continue;
		}
		if (depth !== 0) continue;
		const operators = ["===", "!==", "==", "!=", ">=", "<=", ">", "<"];
		for (const op of operators) {
			if (text.slice(i, i + op.length) === op) return { op, at: i };
		}
	}
	return null;
}

/** `x` 后接 `.prop` / `[index]` / `["key"]` 的路径链。 */
function parsePath(source, param) {
	const text = source.trim();
	if (text.slice(0, param.length) !== param) {
		return { ok: false, error: "表达式必须以 " + param + " 开头" };
	}
	let i = param.length;
	const segments = [];
	while (i < text.length) {
		const c = text.charAt(i);
		if (c === ".") {
			const m = /^[A-Za-z_$][\w$]*/.exec(text.slice(i + 1));
			if (!m) return { ok: false, error: "期望属性名" };
			segments.push({ prop: m[0] });
			i += 1 + m[0].length;
		} else if (c === "[") {
			const close = text.indexOf("]", i + 1);
			if (close < 0) return { ok: false, error: "缺少 ]" };
			const inner = text.slice(i + 1, close).trim();
			if (/^-?\d+$/.test(inner)) segments.push({ index: Number(inner) });
			else {
				const a = inner.charAt(0);
				const b = inner.charAt(inner.length - 1);
				if ((a === '"' && b === '"') || (a === "'" && b === "'"))
					segments.push({ prop: inner.slice(1, -1) });
				else return { ok: false, error: "下标只能是数字或字符串" };
			}
			i = close + 1;
		} else {
			return { ok: false, error: "无法识别的字符 " + JSON.stringify(c) };
		}
	}
	return {
		ok: true,
		get: (root) => {
			let current = root;
			for (const segment of segments) {
				if (segment.prop !== undefined) {
					const step = readProp(current, segment.prop);
					if (!step.ok) throw new FilterError(step.error);
					current = step.value;
				} else {
					const step = readIndex(current, segment.index);
					if (!step.ok) throw new FilterError(step.error);
					current = step.value;
				}
			}
			return current;
		},
	};
}

function parseLiteral(source) {
	const text = source.trim();
	if (text === "true") return { ok: true, value: true };
	if (text === "false") return { ok: true, value: false };
	if (text === "null") return { ok: true, value: null };
	if (/^-?\d+(\.\d+)?$/.test(text)) return { ok: true, value: Number(text) };
	const a = text.charAt(0);
	const b = text.charAt(text.length - 1);
	if ((a === '"' && b === '"') || (a === "'" && b === "'")) {
		return { ok: true, value: text.slice(1, -1) };
	}
	return { ok: false, error: "不支持的比较值 " + text };
}

function compare(left, operator, right) {
	switch (operator) {
		case "===":
			return left === right;
		case "!==":
			return left !== right;
		case "==":
			return left == right; // eslint-disable-line eqeqeq
		case "!=":
			return left != right; // eslint-disable-line eqeqeq
		case ">":
			return left > right;
		case ">=":
			return left >= right;
		case "<":
			return left < right;
		case "<=":
			return left <= right;
		default:
			return false;
	}
}

function truthy(value) {
	return !!value;
}
