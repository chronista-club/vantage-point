#!/usr/bin/env python3
"""assets/brand/source.svgから全消費先のアイコンを生成・検証する。"""
from pathlib import Path
import argparse
import copy
import hashlib
import json
import shutil
import struct
import subprocess
import tempfile
import xml.etree.ElementTree as ET

REPO = Path(__file__).resolve().parents[1]
NS = 'http://www.w3.org/2000/svg'
ET.register_namespace('', NS)


def generate():
    source = REPO/'assets/brand/source.svg'
    license_text = (REPO/'assets/brand/PHOSPHOR-LICENSE.txt').read_text()
    root = ET.fromstring(source.read_bytes())
    mark = root.find('.//{'+NS+'}g[@id="vp-mark"]')
    if mark is None:
        raise ValueError('source.svg must contain the canonical vp-mark group')
    # コピーされた単独SVGにもライセンスを持たせる。
    metadata = ET.Element('{'+NS+'}metadata')
    metadata.text = 'Vantage Point — Phosphor Mountainsを元に再設計。\n'+license_text
    root.insert(0, copy.deepcopy(metadata))
    app_svg = ET.tostring(root, encoding='utf-8', xml_declaration=True)
    # Legacy macOS ICNS / NSImage need their own transparent outer canvas.
    # Keep the canonical artwork and Web/Windows outputs full-bleed.
    mac_root = ET.Element('{'+NS+'}svg', {'viewBox':'0 0 1024 1024','width':'1024','height':'1024'})
    mac_artwork = copy.deepcopy(root)
    mac_artwork.attrib.update({'x':'100','y':'100','width':'824','height':'824'})
    mac_root.append(mac_artwork)
    mac_svg = ET.tostring(mac_root, encoding='utf-8', xml_declaration=True)
    logo = ET.Element('{'+NS+'}svg', {'viewBox':'0 0 256 256','width':'256','height':'256'})
    logo.append(metadata)
    logo.append(copy.deepcopy(mark))
    mark_svg = ET.tostring(logo, encoding='utf-8', xml_declaration=True)
    renderer = shutil.which('resvg') or '/opt/homebrew/bin/resvg'
    renderer_version = subprocess.check_output([renderer,'--version'], text=True).strip()
    with tempfile.TemporaryDirectory(prefix='vp-brand-') as directory:
        tmp = Path(directory)
        (tmp/'app.svg').write_bytes(app_svg)
        images = {}
        for size in [16,24,32,48,64,128,180,256,512,1024]:
            target = tmp/f'{size}.png'
            subprocess.run([renderer,'--width',str(size),str(tmp/'app.svg'),str(target)],check=True)
            images[size] = target.read_bytes()
        (tmp/'mac.svg').write_bytes(mac_svg)
        mac_images = {}
        for size in [16,32,64,128,256,512,1024]:
            target = tmp/f'mac-{size}.png'
            subprocess.run([renderer,'--width',str(size),str(tmp/'mac.svg'),str(target)],check=True)
            mac_images[size] = target.read_bytes()
    # PNG圧縮エントリ: macOSとWindows Vista以降がサポートする形式。
    chunks = []
    for kind,size in [('icp4',16),('icp5',32),('icp6',64),('ic07',128),('ic08',256),('ic09',512),('ic10',1024),('ic11',32),('ic12',64),('ic13',256),('ic14',512)]:
        payload = mac_images[size]
        chunks.append(kind.encode()+struct.pack('>I',len(payload)+8)+payload)
    icns_body = b''.join(chunks)
    icns = b'icns'+struct.pack('>I',len(icns_body)+8)+icns_body
    sizes = [16,24,32,48,64,128,256]
    offset = 6+16*len(sizes)
    entries = []
    for size in sizes:
        entries.append(struct.pack('<BBBBHHII',size%256,size%256,0,0,1,32,len(images[size]),offset))
        offset += len(images[size])
    ico = struct.pack('<HHH',0,1,len(sizes))+b''.join(entries)+b''.join(images[s] for s in sizes)
    web = {'mark.svg':mark_svg,'app-icon.svg':app_svg,'app-icon.png':images[1024],
           'favicon.png':images[32],'apple-touch-icon.png':images[180],
           'PHOSPHOR-LICENSE.txt':license_text.encode()}
    manifest = {'version':1,'status':'adopted','source':'assets/brand/source.svg',
                'sourceSha256':hashlib.sha256(source.read_bytes()).hexdigest(),
                'design':'docs/design/74-brand-identity.md',
                'renderer':renderer_version,
                'files':{name:hashlib.sha256(data).hexdigest() for name,data in web.items()}}
    web['manifest.json']=(json.dumps(manifest,ensure_ascii=False,indent=2)+'\n').encode()
    native = {'icon.svg':app_svg,'icon.png':images[1024],'icon.icns':icns,'icon.ico':ico,
              'icon-macos.svg':mac_svg,'icon-macos.png':mac_images[1024],
              'PHOSPHOR-LICENSE.txt':license_text.encode()}
    return web,native


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check',action='store_true',help='差分を報告するだけで書き換えない')
    parser.add_argument('--portal',type=Path,help='同期するportalリポジトリのパス')
    parser.add_argument('--output-root',type=Path,default=REPO,help='出力先（検証用に変更可能）')
    args=parser.parse_args()
    web,native=generate()
    outputs={args.output_root/'assets/brand/generated'/name:data for name,data in web.items()}
    outputs.update({args.output_root/'crates/vp-app/assets'/name:data for name,data in native.items()})
    if args.portal:
        outputs.update({args.portal/'public/brand/v1'/name:data for name,data in web.items()})
        outputs[args.portal/'public/favicon.svg']=web['app-icon.svg']
    changed=[]
    for path,data in outputs.items():
        if path.exists() and path.read_bytes()==data:
            continue
        changed.append(path)
        if not args.check:
            path.parent.mkdir(parents=True,exist_ok=True)
            path.write_bytes(data)
    if args.check and changed:
        for path in changed: print(f'DRIFT: {path}')
        return 1
    print(f'{"Verified" if args.check else "Generated"} {len(outputs)} assets; {len(changed)} changed')
    return 0


if __name__=='__main__':
    raise SystemExit(main())
