#!/usr/bin/env python3
import asyncio
import json
import urllib.request
import websockets

BRAVE_CDP = 'http://127.0.0.1:9224'
PROXY_PORT = 9222

async def get_target_url():
    while True:
        try:
            with urllib.request.urlopen(f'{BRAVE_CDP}/json/version', timeout=5) as r:
                return json.loads(r.read())['webSocketDebuggerUrl']
        except Exception:
            await asyncio.sleep(1)

async def proxy(ws):
    target_url = await get_target_url()
    async with websockets.connect(target_url) as target:
        async def forward(src, dst):
            async for msg in src:
                await dst.send(msg)
        await asyncio.gather(forward(ws, target), forward(target, ws))

async def main():
    start_server = await websockets.serve(proxy, '0.0.0.0', PROXY_PORT)
    await start_server.wait_closed()

asyncio.run(main())
