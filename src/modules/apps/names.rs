//! 应用显示名和搜索关键词：从 `InfoPlist.strings` / `Info.plist` 里取本地化
//! 名称、bundle identifier 和 URL schemes，让 Launcher 搜索既支持本地化名
//! （「微信」）也支持原始名和别名（WeChat / weixin）。
//!
//! macOS 上 Finder 对 `/Applications/WeChat.app` 显示的是「微信」而不是
//! WeChat——那个中文名字来自 bundle 里的 `zh-Hans.lproj/InfoPlist.strings`
//! 对 `CFBundleDisplayName` 的覆盖。`Info.plist` 本身只写了 `CFBundleName`
//! = "WeChat"，`NSBundle objectForInfoDictionaryKey:` 在非 AppKit 进程里
//! 也不会自动走本地化查找。
//!
//! 这里直接读 `InfoPlist.strings`：`NSDictionary dictionaryWithContentsOfFile:`
//! 能识别 binary plist / XML plist / .strings 三种格式。语言优先级从系统
//! AppleLanguages 读，再加上 zh-Hans / en / Base 兜底。找不到或读失败时
//! 调回文件名（`WeChat.app` → `WeChat`），保证不会返回空。

use std::collections::HashMap;
use std::path::Path;

/// 静态别名表：按 bundle identifier 挂用户常用但 App 自身元数据里没有的
/// 搜索词。Raycast 也维护了类似的一份——Info.plist / URL schemes 不会给
/// 出「weixin」这种跨产品线的别名，所以需要硬编码。
///
/// 只放高频的几个，其他 case 让 App 自身的 raw_names + URL schemes 覆盖。
/// 格式：`bundle_id → [alias1, alias2, ...]`。
#[cfg(target_os = "macos")]
static STATIC_ALIASES: std::sync::OnceLock<HashMap<&'static str, &'static [&'static str]>> =
    std::sync::OnceLock::new();

#[cfg(target_os = "macos")]
fn static_aliases() -> &'static HashMap<&'static str, &'static [&'static str]> {
    STATIC_ALIASES.get_or_init(|| {
        let mut m = HashMap::new();
        // 企业微信：Info.plist 里完全没有 weixin，但用户经常混用
        m.insert(
            "com.tencent.WeWorkMac",
            &["weixin", "wechat", "企业微信", "企微", "workwechat"] as &'static [&'static str],
        );
        // 微信：URL schemes 已有 weixin/wechat，但补上中文别名
        m.insert(
            "com.tencent.xinWeChat",
            &["微信", "wx"] as &'static [&'static str],
        );
        // 飞书 / Lark
        m.insert(
            "com.larksuite.lark",
            &["feishu", "飞书", "lark"] as &'static [&'static str],
        );
        // 钉钉
        m.insert(
            "com.alibaba.DingTalkMac",
            &["dingtalk", "ding", "dd", "钉钉"] as &'static [&'static str],
        );
        // QQ
        m.insert(
            "com.tencent.QQMac",
            &["qq", "tencentqq", "腾讯qq"] as &'static [&'static str],
        );
        // 微信读书
        m.insert(
            "com.tencent.weread",
            &["weread", "wechatread", "微信读书"] as &'static [&'static str],
        );
        m
    })
}

/// 对应用数组逐个填显示名和搜索关键词；原地修改 `AppEntry.name` 与
/// `AppEntry.keywords`。
///
/// 非 macOS 或 objc 调用整体失败时静默跳过：所有 entry 保持文件名和空
/// keywords，调用方不需要做额外处理。
pub(crate) fn fill_display_names_and_keywords(apps: &mut [crate::modules::apps::scan::AppEntry]) {
    #[cfg(target_os = "macos")]
    {
        let langs = preferred_languages();
        for app in apps.iter_mut() {
            let info = match bundle_info(&app.path, &langs) {
                Some(info) => info,
                None => continue,
            };
            if let Some(name) = info.display_name {
                app.name = name;
            }
            // original_name 已在 scan 时填好，不要再覆盖。
            let mut kws = Vec::new();
            if let Some(bundle_id) = info.bundle_id.as_ref() {
                kws.push(bundle_id.clone());
                // 静态别名表：Info.plist 和 URL schemes 里没有、但用户经常
                // 用来搜这个 App 的名称（例如 weixin 搜企业微信）。
                #[cfg(target_os = "macos")]
                {
                    if let Some(extra) = static_aliases().get(bundle_id.as_str()) {
                        kws.extend(extra.iter().map(|s| (*s).to_string()));
                    }
                }
            }
            kws.extend(info.url_schemes);
            // raw_names：Info.plist 原始英文名（如 WeCom）作为额外搜索别名；
            // 对于文件名是中文的 App 特别重要。
            for n in info.raw_names {
                if n != app.original_name {
                    kws.push(n);
                }
            }
            app.keywords = kws;
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = apps;
    }
}

/// 一次 ObjC 调用拿到一个 `.app` 的所有 bundle 信息。
///
/// 用一个 autoreleasepool 包住整个流程，避免每个入口函数单独创建池；
/// 共用 bundle 对象，减少 `bundleWithPath:` 的开销。
#[cfg(target_os = "macos")]
#[derive(Default)]
struct BundleInfo {
    /// 本地化后的显示名（CFBundleDisplayName / CFBundleName），可能是中文。
    display_name: Option<String>,
    /// Info.plist 里原始值（未本地化）的 CFBundleDisplayName + CFBundleName。
    /// 用来做搜索别名：例如 `/Applications/企业微信.app` 的文件名是中文，
    /// 但 Info.plist 原始 CFBundleDisplayName 是 WeCom——用户搜 WeCom 应该能命中。
    raw_names: Vec<String>,
    /// CFBundleIdentifier，如 `com.tencent.xinWeChat`。
    bundle_id: Option<String>,
    /// CFBundleURLTypes 里的所有 CFBundleURLSchemes，如 `["xweixin", "weixin", "wechat"]`。
    url_schemes: Vec<String>,
}

#[cfg(target_os = "macos")]
fn bundle_info(app_path: &Path, langs: &[String]) -> Option<BundleInfo> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};

    use crate::ui::mac_ffi::{ns_string, ns_string_to_rust};

    autoreleasepool(|_| unsafe {
        let bundle_class = AnyClass::get(c"NSBundle")?;
        let path_ns = ns_string(&app_path.to_string_lossy());
        if path_ns.is_null() {
            return None;
        }
        let bundle: *mut AnyObject = msg_send![bundle_class, bundleWithPath: path_ns];
        if bundle.is_null() {
            return None;
        }

        let mut info = BundleInfo::default();

        // --- 先读 Info.plist 原始值：塞进 raw_names 留作搜索别名 ---
        let info_dict: *mut AnyObject = msg_send![bundle, infoDictionary];
        if !info_dict.is_null() {
            if let Some(n) = dict_lookup_string(info_dict, "CFBundleDisplayName") {
                info.raw_names.push(n);
            }
            if let Some(n) = dict_lookup_string(info_dict, "CFBundleName") {
                info.raw_names.push(n);
            }
            info.bundle_id = dict_lookup_string(info_dict, "CFBundleIdentifier");
            info.url_schemes = extract_url_schemes(info_dict);
        }

        // --- 遍历所有有 InfoPlist.strings 的 lproj（按用户语言优先级）：
        //     * 每个 lproj 里的 CFBundleDisplayName/CFBundleName 都塞进 raw_names
        //       当搜索别名（例如 WeCom 的 en.lproj 覆盖值就是 WeCom，不管用户
        //       语言最终选了哪个 display_name，英文名 WeCom 都应该可搜）。
        //     * 第一个命中的 lproj 决定最终 display_name。
        //     * 全部 lproj 都没有 → display_name 留在 None，回退到 raw_names[0] 或文件名。
        for lang in langs {
            let key = ns_string(lang);
            let info_plist = ns_string("InfoPlist");
            let strings = ns_string("strings");
            let nil = std::ptr::null_mut::<AnyObject>();
            let strings_path: *mut AnyObject = msg_send![
                bundle,
                pathForResource: info_plist,
                ofType: strings,
                inDirectory: nil,
                forLocalization: key
            ];
            if strings_path.is_null() {
                continue;
            }
            let dict_class = AnyClass::get(c"NSDictionary")?;
            let strings_dict: *mut AnyObject =
                msg_send![dict_class, dictionaryWithContentsOfFile: strings_path];
            if strings_dict.is_null() {
                continue;
            }
            if let Some(disp) = dict_lookup_string(strings_dict, "CFBundleDisplayName") {
                info.raw_names.push(disp.clone());
                if info.display_name.is_none() {
                    info.display_name = Some(disp);
                }
            }
            if let Some(name) = dict_lookup_string(strings_dict, "CFBundleName") {
                info.raw_names.push(name.clone());
                if info.display_name.is_none() {
                    info.display_name = Some(name);
                }
            }
        }

        // 最终回退：所有 lproj + Info.plist 都没给 display_name 的极端情况。
        if info.display_name.is_none() {
            info.display_name = info.raw_names.first().cloned();
        }

        let _ = ns_string_to_rust; // silence unused in some cfgs
        Some(info)
    })
}

/// 从 Info.plist 的 CFBundleURLTypes 数组里抽出所有 CFBundleURLSchemes。
///
/// 结构：`CFBundleURLTypes: [{ CFBundleURLSchemes: ["weixin", "wechat"], ... }]`。
/// 把所有数组里的 scheme 扁平合并成一个 Vec。
#[cfg(target_os = "macos")]
unsafe fn extract_url_schemes(info_dict: *mut objc2::runtime::AnyObject) -> Vec<String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    let key = crate::ui::mac_ffi::ns_string("CFBundleURLTypes");
    if key.is_null() {
        return Vec::new();
    }
    let url_types: *mut AnyObject = msg_send![info_dict, objectForKey: key];
    if url_types.is_null() {
        return Vec::new();
    }

    // 先假设它是 NSArray；不是就直接返回空。
    let array_class = match AnyClass::get(c"NSArray") {
        Some(cls) => cls,
        None => return Vec::new(),
    };
    let is_array: bool = msg_send![url_types, isKindOfClass: array_class];
    if !is_array {
        return Vec::new();
    }

    let mut schemes = Vec::new();
    let type_count: usize = msg_send![url_types, count];
    for i in 0..type_count {
        let url_type: *mut AnyObject = msg_send![url_types, objectAtIndex: i];
        if url_type.is_null() {
            continue;
        }
        let schemes_key = crate::ui::mac_ffi::ns_string("CFBundleURLSchemes");
        if schemes_key.is_null() {
            continue;
        }
        let schemes_arr: *mut AnyObject = msg_send![url_type, objectForKey: schemes_key];
        if schemes_arr.is_null() {
            continue;
        }
        let arr_count: usize = msg_send![schemes_arr, count];
        for j in 0..arr_count {
            let item: *mut AnyObject = msg_send![schemes_arr, objectAtIndex: j];
            if let Some(s) = crate::ui::mac_ffi::ns_string_to_rust(item) {
                schemes.push(s);
            }
        }
    }
    schemes
}

/// `NSDictionary` → 字符串键 → 取值并转成 Rust String。
///
/// ObjC 字典里值可能是 `NSString`、`NSNumber`、`NSArray` 等；只接受
/// `NSString`（转 UTF-8 Rust String），其他类型一律返回 None。
#[cfg(target_os = "macos")]
unsafe fn dict_lookup_string(dict: *mut objc2::runtime::AnyObject, key: &str) -> Option<String> {
    use objc2::msg_send;

    let key_ns = crate::ui::mac_ffi::ns_string(key);
    if key_ns.is_null() {
        return None;
    }
    let value: *mut objc2::runtime::AnyObject = msg_send![dict, objectForKey: key_ns];
    if value.is_null() {
        return None;
    }
    crate::ui::mac_ffi::ns_string_to_rust(value)
}

/// 从 `NSUserDefaults standardUserDefaults` 的 `AppleLanguages` 读出系统
/// 语言列表，再拼上 Base / zh-Hans / en 兜底，返回一个可喂给
/// `pathForResource:forLocalization:` 的有序数组。
///
/// 关键细节：兜底时 **Base 排在 en 前面**。原因是有些 App（例如企业微信）
/// 的 zh-Hans.lproj 目录存在但里面没有 InfoPlist.strings，而 en.lproj 里有
/// 英文覆盖值。我们要优先命中 Base.lproj 里的中文版本，而不是退到 en。
/// Base.lproj 语义上就是「没有任何本地化时该用什么」，优先级高于 development
/// region 的 en。
///
/// 命令行进程里 NSUserDefaults 的 AppleLanguages 可能读不到（系统默认值
/// 还没加载），这时 fallback 到 `["Base", "zh-Hans", "en"]`。
#[cfg(target_os = "macos")]
fn preferred_languages() -> Vec<String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use crate::ui::mac_ffi::ns_string_to_rust;

    let mut langs: Vec<String> = Vec::new();
    unsafe {
        if let Some(user_defaults) = AnyClass::get(c"NSUserDefaults") {
            let defaults: *mut AnyObject = msg_send![user_defaults, standardUserDefaults];
            if !defaults.is_null() {
                let apple_langs_ns: *mut AnyObject = msg_send![defaults, objectForKey: crate::ui::mac_ffi::ns_string("AppleLanguages")];
                if !apple_langs_ns.is_null() {
                    // NSArray → 逐个取元素
                    let count: usize = msg_send![apple_langs_ns, count];
                    for i in 0..count {
                        let item: *mut AnyObject = msg_send![apple_langs_ns, objectAtIndex: i];
                        if let Some(s) = ns_string_to_rust(item) {
                            langs.push(s);
                        }
                    }
                }
            }
        }
    }
    // 兜底：永远要能找到 Base / zh-Hans / en。
    // Base 优先于 en —— 有些 App zh-Hans.lproj 目录存在但没有 InfoPlist.strings，
    // en.lproj 里却有英文覆盖值；这时应该命中 Base.lproj 里的正确版本。
    for fallback in ["Base", "zh-Hans", "en"] {
        if !langs.iter().any(|l| l == fallback) {
            langs.push(fallback.to_string());
        }
    }
    langs
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn fill_names_is_noop_on_other_platforms() {
        #[cfg(not(target_os = "macos"))]
        {
            let mut apps: Vec<crate::modules::apps::scan::AppEntry> = Vec::new();
            fill_display_names_and_keywords(&mut apps);
            assert!(apps.is_empty());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wechat_gets_localized_name_and_url_schemes() {
        use crate::modules::apps::scan::AppEntry;
        let mut apps = vec![AppEntry {
            path: PathBuf::from("/Applications/WeChat.app"),
            name: "WeChat".to_string(),
            original_name: "WeChat".to_string(),
            subtitle: "应用".to_string(),
            keywords: Vec::new(),
        }];
        fill_display_names_and_keywords(&mut apps);

        // 本地化显示名：zh-Hans.lproj 里是「微信」。
        assert_eq!(apps[0].name, "微信");
        // original_name 不应该被覆盖。
        assert_eq!(apps[0].original_name, "WeChat");
        // bundle id 应包含 com.tencent.xinWeChat。
        assert!(
            apps[0]
                .keywords
                .iter()
                .any(|k| k == "com.tencent.xinWeChat"),
            "缺少 bundle id：{:?}",
            apps[0].keywords
        );
        // URL schemes 应包含 wechat / weixin / xweixin。
        let schemes: Vec<&str> = apps[0]
            .keywords
            .iter()
            .filter(|k| !k.contains('.'))
            .map(|s| s.as_str())
            .collect();
        assert!(
            schemes.contains(&"wechat"),
            "缺少 wechat scheme：{:?}",
            schemes
        );
        assert!(
            schemes.contains(&"weixin"),
            "缺少 weixin scheme：{:?}",
            schemes
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn safari_keeps_filename_without_localization() {
        use crate::modules::apps::scan::AppEntry;
        let mut apps = vec![AppEntry {
            path: PathBuf::from("/Applications/Safari.app"),
            name: "Safari".to_string(),
            original_name: "Safari".to_string(),
            subtitle: "系统应用".to_string(),
            keywords: Vec::new(),
        }];
        fill_display_names_and_keywords(&mut apps);

        // Safari 没有 InfoPlist.strings 的覆盖，应保持 Safari。
        assert_eq!(apps[0].name, "Safari");
        // 有 bundle id。
        assert!(
            apps[0].keywords.iter().any(|k| k == "com.apple.Safari"),
            "缺少 bundle id：{:?}",
            apps[0].keywords
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn wecom_uses_base_lproj_chinese_name() {
        // 企业微信：zh-Hans.lproj 目录存在但 InfoPlist.strings 不在里面，
        // en.lproj 有英文覆盖 WeCom，Base.lproj 有中文"企业微信"。
        // Base 优先于 en，所以应该命中 Base 的中文版本。
        use crate::modules::apps::scan::AppEntry;
        let mut apps = vec![AppEntry {
            path: PathBuf::from("/Applications/企业微信.app"),
            name: "企业微信".to_string(),
            original_name: "企业微信".to_string(),
            subtitle: "应用".to_string(),
            keywords: Vec::new(),
        }];
        fill_display_names_and_keywords(&mut apps);

        assert_eq!(apps[0].name, "企业微信");
        assert!(
            apps[0]
                .keywords
                .iter()
                .any(|k| k == "com.tencent.WeWorkMac"),
            "缺少 bundle id：{:?}",
            apps[0].keywords
        );
        // raw_names：Info.plist 原始 CFBundleDisplayName 是 WeCom，
        // 必须进 keywords，这样搜 wecom 也能命中。
        assert!(
            apps[0].keywords.iter().any(|k| k == "WeCom"),
            "缺少 WeCom raw_name：{:?}",
            apps[0].keywords
        );
        // 静态别名：weixin 不在 Info.plist 里，但用户经常混用，
        // 必须能搜到企业微信。
        assert!(
            apps[0].keywords.iter().any(|k| k == "weixin"),
            "缺少 weixin 静态别名：{:?}",
            apps[0].keywords
        );
    }
}
