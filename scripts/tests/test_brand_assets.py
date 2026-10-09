"""ブランドの生成・配信契約と、手編集の検出を検証する。"""
from pathlib import Path
import json
import struct
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

SCRIPT = Path(__file__).resolve().parents[1] / 'brand_assets.py'


class BrandAssetsTest(unittest.TestCase):
    def run_generator(self, root, *args):
        return subprocess.run([sys.executable, str(SCRIPT), '--output-root', str(root), *args], capture_output=True, text=True)

    def test_generated_native_and_web_assets_share_source(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            portal = root / 'portal'
            result = self.run_generator(root, '--portal', str(portal))
            self.assertEqual(result.returncode, 0, result.stderr)
            native = root / 'crates/vp-app/assets'
            web = root / 'assets/brand/generated'
            self.assertEqual((native/'icon.svg').read_bytes(), (web/'app-icon.svg').read_bytes())
            self.assertEqual((native/'icon.png').read_bytes(), (web/'app-icon.png').read_bytes())
            self.assertEqual((portal/'public/favicon.svg').read_bytes(), (web/'app-icon.svg').read_bytes())
            self.assertEqual((portal/'public/brand/v1/mark.svg').read_bytes(), (web/'mark.svg').read_bytes())
            self.assertIn(b'Phosphor Icons', (portal/'public/brand/v1/PHOSPHOR-LICENSE.txt').read_bytes())
            png = (native/'icon.png').read_bytes()
            self.assertEqual(struct.unpack('>II', png[16:24]), (1024,1024))
            ico = (native/'icon.ico').read_bytes()
            self.assertEqual(struct.unpack('<HHH', ico[:6]), (0,1,7))
            for i in range(7):
                _,_,_,_,planes,depth,length,offset=struct.unpack_from('<BBBBHHII',ico,6+i*16)
                self.assertEqual((planes,depth),(1,32))
                self.assertEqual(ico[offset:offset+8],b'\x89PNG\r\n\x1a\n')
                self.assertLessEqual(offset+length,len(ico))
            icns=(native/'icon.icns').read_bytes()
            self.assertEqual(icns[:4],b'icns')
            self.assertEqual(struct.unpack('>I',icns[4:8])[0],len(icns))
            self.assertIn(b'ic10',icns)
            # Mac uses an inset canvas; Web/Windows retain the full-bleed image.
            mac_png = native/'icon-macos.png'
            self.assertTrue(mac_png.exists(), 'Dock needs a dedicated padded PNG')
            self.assertNotEqual(mac_png.read_bytes(), png)
            self.assertIn(mac_png.read_bytes(),icns)
            self.assertNotIn(png,icns)
            mac_svg = ET.parse(native/'icon-macos.svg').getroot()
            artwork = mac_svg.find('{http://www.w3.org/2000/svg}svg')
            self.assertIsNotNone(artwork)
            self.assertEqual((artwork.get('x'), artwork.get('y')), ('100', '100'))
            self.assertEqual((artwork.get('width'), artwork.get('height')), ('824', '824'))
            manifest=json.loads((web/'manifest.json').read_text())
            self.assertEqual(manifest['status'],'adopted')
            self.assertEqual(manifest['source'],'assets/brand/source.svg')
            self.assertNotIn('approvedStudy',manifest, 'old study must not be presented as current approval')
            self.assertEqual(manifest['design'],'docs/design/74-brand-identity.md')

    def test_check_detects_drift_without_rewriting_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root=Path(tmp)
            result=self.run_generator(root)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(self.run_generator(root,'--check').returncode,0)
            path=root/'crates/vp-app/assets/icon.png'
            path.write_bytes(b'accidental edit')
            check=self.run_generator(root,'--check')
            self.assertEqual(check.returncode,1,check.stdout+check.stderr)
            self.assertIn('icon.png',check.stdout)
            self.assertEqual(path.read_bytes(),b'accidental edit')


if __name__=='__main__':
    unittest.main()
