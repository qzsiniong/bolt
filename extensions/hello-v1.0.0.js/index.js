import { typeStr, confirm } from 'bolt';

let count = 0;

export async function sayHello({ command, plugin, root, argv }) {
  console.log("xxx")

  // typeStr("123", 1000, 'typeStrtypeStrtypeStr')
  //
  const r = await confirm(`count: ${count++}`)

  console.info("info", r)

  // command: "hello"       plugin: { id: "demo", version: "1.0.0" }
  // root: 插件目录绝对路径   argv: 调用时传入的参数数组
  return `你好，这是 ${plugin.id} v${plugin.version} 的 ${command} 命令`;
}
