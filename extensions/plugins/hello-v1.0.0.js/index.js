import { typeStr } from 'bolt';

export function sayHello({ command, plugin, root, argv }) {
  console.log("xxx")
  console.info("info")

  __bolt.typeStr("123", 1000, 'typeStrtypeStrtypeStr')

  // command: "hello"       plugin: { id: "demo", version: "1.0.0" }
  // root: 插件目录绝对路径   argv: 调用时传入的参数数组
  return `你好，这是 ${plugin.id} v${plugin.version} 的 ${command} 命令`;
}
