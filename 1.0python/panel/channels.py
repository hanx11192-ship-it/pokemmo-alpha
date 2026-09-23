# -*- coding: utf-8 -*-
"""
分发渠道：把生成好的播报内容发到各个渠道。

设计要点：
  - 渠道存在 SQLite（panel.db 的 channels 表），面板可增删改、启停、测试。
  - 三类：wxpusher / webhook / serverchan。
  - 每个渠道单独 try/except，一个渠道挂了不影响其它渠道。
  - 渠道表为空时，回退到老行为（src.core.notify，读环境变量的 WxPusher），
    保证升级后原有推送不会突然断掉。
"""
import os
import json
import logging
import time

import requests

import panel.db as db

logger = logging.getLogger("channels")


class ChannelError(RuntimeError):
    pass


# 渠道类型元信息：面板据此渲染表单
CHANNEL_TYPES = {
    "wxpusher": {
        "label": "WxPusher 微信推送",
        "desc": "通过 WxPusher 公众号推送到微信",
        "fields": [
            {"key": "app_token", "label": "App Token", "type": "password",
             "placeholder": "AT_xxx（留空则读环境变量 WXPUSHER_APP_TOKEN_alpha）"},
            {"key": "topic_ids", "label": "主题 ID", "type": "text",
             "placeholder": "45385，多个用逗号分隔"},
        ],
    },
    "webhook": {
        "label": "Webhook（通用 JSON）",
        "desc": "向任意 URL POST 一段 JSON，可自定义字段结构",
        "fields": [
            {"key": "url", "label": "Webhook 地址", "type": "text", "placeholder": "https://..."},
            {"key": "template", "label": "Body 模板（JSON）", "type": "textarea",
             "placeholder": '{"msgtype":"text","text":{"content":"{content}"}}'},
            {"key": "headers", "label": "额外请求头（JSON，可选）", "type": "textarea",
             "placeholder": '{"Authorization":"Bearer xxx"}'},
        ],
    },
    "serverchan": {
        "label": "Server 酱",
        "desc": "通过 Server 酱（sctapi）推送到微信服务号",
        "fields": [
            {"key": "send_key", "label": "SendKey", "type": "password", "placeholder": "SCTxxxx"},
        ],
    },
}


def _render(tpl: str, content: str, summary: str) -> str:
    # 把 content/summary 转义为「合法的 JSON 字符串片段」（去掉首尾引号）。
    # 这样即使正文里出现英文双引号、反斜杠、换行等字符，替换进模板后整体
    # 仍是合法 JSON，避免下游（如 qq-bridge）解析报 "bad json"。
    safe_content = json.dumps(content or "")[1:-1]
    safe_summary = json.dumps(summary or "")[1:-1]
    return (tpl or "").replace("{content}", safe_content).replace("{summary}", safe_summary)


def _parse_ids(v):
    if isinstance(v, list):
        return [int(x) for x in v if str(x).strip()]
    out = []
    for part in str(v or "").replace("，", ",").split(","):
        part = part.strip()
        if part:
            try:
                out.append(int(part))
            except ValueError:
                pass
    return out


def _send_wxpusher(cfg: dict, content: str, summary: str):
    token = (cfg.get("app_token") or "").strip()
    if not token:
        token = os.environ.get("WXPUSHER_APP_TOKEN_alpha", "")
    if not token:
        raise ChannelError("缺少 App Token（未填写，环境变量 WXPUSHER_APP_TOKEN_alpha 也未设置）")
    payload = {
        "appToken": token,
        "content": content,
        "summary": summary or "头目",
        "contentType": 1,
        "topicIds": _parse_ids(cfg.get("topic_ids")),
    }
    resp = requests.post(
        "https://wxpusher.zjiecode.com/api/send/message",
        json=payload, timeout=20,
    )
    resp.raise_for_status()
    data = resp.json()
    if isinstance(data, dict) and data.get("code") not in (1000, None):
        raise ChannelError(f"WxPusher 业务错误: {data}")
    return data


def _send_webhook(cfg: dict, content: str, summary: str):
    url = (cfg.get("url") or "").strip()
    if not url:
        raise ChannelError("未填写 Webhook 地址")
    tpl = (cfg.get("template") or "").strip() or '{"content": "{content}", "summary": "{summary}"}'
    body_text = _render(tpl, content, summary)
    try:
        body = json.loads(body_text)
    except Exception:
        # 模板不是合法 JSON 时，退化为纯文本提交
        body = None
    headers = {"Content-Type": "application/json"}
    extra = cfg.get("headers")
    if extra:
        try:
            headers.update(json.loads(extra) if isinstance(extra, str) else extra)
        except Exception:
            pass
    if body is not None:
        resp = requests.post(url, json=body, headers=headers, timeout=20)
    else:
        resp = requests.post(url, data=body_text.encode("utf-8"),
                             headers={"Content-Type": "text/plain; charset=utf-8"}, timeout=20)
    resp.raise_for_status()
    return {"status_code": resp.status_code, "body": resp.text[:300]}


def _send_serverchan(cfg: dict, content: str, summary: str):
    key = (cfg.get("send_key") or "").strip()
    if not key:
        raise ChannelError("未填写 SendKey")
    resp = requests.post(
        f"https://sctapi.ftqq.com/{key}.send",
        data={"title": summary or "头目", "desp": content}, timeout=20,
    )
    resp.raise_for_status()
    data = resp.json()
    if isinstance(data, dict) and data.get("code") not in (0, None):
        raise ChannelError(f"Server酱错误: {data}")
    return data


_SENDERS = {
    "wxpusher": _send_wxpusher,
    "webhook": _send_webhook,
    "serverchan": _send_serverchan,
}


def send_one(ch: dict, content: str, summary: str = None) -> dict:
    """向单个渠道发送。返回 {ok, error}。"""
    ctype = ch.get("type")
    fn = _SENDERS.get(ctype)
    if not fn:
        return {"ok": False, "error": f"未知渠道类型: {ctype}"}
    try:
        fn(ch.get("config") or {}, content, summary or "")
        return {"ok": True}
    except Exception as e:
        return {"ok": False, "error": str(e)}


def send_all(content: str, summary: str = None) -> dict:
    """向所有启用渠道发送。

    返回 {"sent": n, "failed": n, "details": [...]}。
    全部失败时抛 ChannelError，让调用方不写去重标记（下轮重试）。
    渠道表为空时回退到 src.core.notify。
    """
    chs = [c for c in db.list_channels() if c.get("enabled")]
    if not chs:
        from src.core.notify import send as legacy_send
        legacy_send(content, summary)
        return {"sent": 1, "failed": 0, "details": [{"name": "WxPusher(环境变量)", "ok": True}],
                "legacy": True}

    details, ok_n, fail_n = [], 0, 0
    for ch in chs:
        r = send_one(ch, content, summary)
        r["name"] = ch.get("name")
        r["type"] = ch.get("type")
        details.append(r)
        if r["ok"]:
            ok_n += 1
            db.log_event("info", "channel", f"推送成功：{ch.get('name')}", source=ch.get("type", ""))
        else:
            fail_n += 1
            logger.warning("渠道 %s 推送失败: %s", ch.get("name"), r.get("error"))
            db.log_event("error", "channel",
                         f"推送失败：{ch.get('name')} - {r.get('error')}",
                         source=ch.get("type", ""))
    if ok_n == 0:
        raise ChannelError("；".join(f"{d['name']}: {d.get('error')}" for d in details))
    return {"sent": ok_n, "failed": fail_n, "details": details}


def test_channel(ch: dict) -> dict:
    """发一条测试消息。"""
    stamp = time.strftime("%Y-%m-%d %H:%M:%S")
    content = f"【Alpha 面板 · 测试推送】\n时间：{stamp}\n渠道：{ch.get('name')}\n\n如果你收到这条消息，说明该渠道配置正确。"
    return send_one(ch, content, "面板测试推送")
