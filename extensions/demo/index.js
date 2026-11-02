// 演示扩展 —— 脚本命令入口。
//
// 命令 id 就是这里导出的函数名；宿主按
// 「具名导出 → export default 对象 → globalThis.bolt.commands」三档查找。
// 每个扩展一个常驻运行时：下面的模块级变量跨命令保留，单条命令 60 秒超时。
//
// 清单 permissions 声明了 ["fs", "net", "typeStr"]：没声明的危险 API
// （exec / env / 未声明的文件与网络调用）会直接抛错——fail-closed。
import {
  readFile,
  listDir,
  fetch,
  confirm,
  pickPath,
  requestFocus,
  typeStr,
  sleep,
} from "bolt";

// 模块级状态：常驻运行时里，下次调用还在。
let visits = 0;

/** hello —— 演示命令上下文与返回值：返回的字符串输出到 Console。 */
export function hello({ command, extension, root, argv }) {
  console.log("argv =", argv);
  return `你好，这是 ${extension.id} v${extension.version} 的 ${command} 命令（扩展目录在 ${root}）`;
}

/** counter —— 模块级状态 + 异步：await sleep 发生在扩展线程，不占宿主主线程。 */
export async function counter() {
  visits += 1;
  await sleep(300);
  return `这是第 ${visits} 次调用（模块级状态跨调用保留）`;
}

/** peek —— fs 权限：读扩展目录自己。 */
export function peek({ root }) {
  const files = listDir(root);
  const manifest = JSON.parse(readFile(`${root}/extension.json`));
  console.info("扩展目录里的文件：", files);
  return `${manifest.name} 声明了 ${manifest.commands.length} 条命令，目录里有 ${files.length} 个文件`;
}

/** weather —— net 权限：fetch 返回 Promise，异步不阻塞。 */
export async function weather() {
  try {
    const { status, body } = await fetch("https://httpbin.org/json");
    if (status !== 200) return `请求失败：HTTP ${status}`;
    const data = JSON.parse(body);
    const title = data.slideshow?.title ?? "(无标题)";
    return `HTTP ${status}：${title}`;
  } catch (error) {
    return `网络不可用：${error}`;
  }
}

/** chat —— 交互 API（confirm / pickPath / requestFocus / typeStr）不需要声明权限。 */
export async function chat() {
  const ok = await confirm("演示：选一个文件，再把它的路径逐字打进你点中的窗口？");
  if (!ok) return "你点了取消";

  const path = await pickPath("挑一个文件（可取消）", "file");
  if (path === null) return "没有选文件";

  requestFocus("点一下要输入到的窗口");
  typeStr(path, 40, "演示输入");
  return `已把 ${path} 逐字打进焦点窗口`;
}
