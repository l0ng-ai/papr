#!/usr/bin/env python3
"""Build an unsigned arm64 IPA directly with the installed iPhoneOS SDK.

This uses the same Tauri Rust/Swift core as the generated Xcode project.
It does not require a simulator runtime, signing identity or provisioning profile.
Run on macOS with Xcode, Rust's aarch64-apple-ios target, Node and dependencies.
"""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shlex
import shutil
import struct
import subprocess
import sys
import tempfile
import zipfile

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / 'target' / 'ios-unsigned'
APP = OUTPUT / 'Payload' / 'Papr.app'
IOS_TARGET = 'aarch64-apple-ios'
MIN_IOS = '15.0'

def run(args, **kwargs):
    print('+', shlex.join(map(str,args)), flush=True)
    return subprocess.run(list(map(str,args)), check=True, cwd=ROOT, **kwargs)

def verify(ipa):
    """Check the delivered archive, not just the intermediate application."""
    with zipfile.ZipFile(ipa) as archive:
        assert archive.testzip() is None, 'IPA CRC check failed'
        names=archive.namelist()
        assert all(name.startswith('Payload/') for name in names)
        assert not any('_CodeSignature/' in name or name.endswith(('embedded.mobileprovision','papr.db','auth.json')) for name in names)
        info=plistlib.loads(archive.read('Payload/Papr.app/Info.plist'))
        assert info['CFBundleIdentifier']=='com.thomas.papr'
        assert info['MinimumOSVersion']==MIN_IOS
        assert info['CFBundleSupportedPlatforms']==['iPhoneOS']
        for key,idiom,required in [('CFBundleIcons','iphone',{(120,120),(180,180)}),('CFBundleIcons~ipad','ipad',{(152,152),(167,167)})]:
            files=info[key]['CFBundlePrimaryIcon']['CFBundleIconFiles']
            sizes=set()
            for stem in files:
                candidates=[name for name in names if name.startswith('Payload/Papr.app/'+stem) and name.endswith('.png')]
                assert candidates, f'Missing {idiom} icon: {stem}'
                for name in candidates:
                    png=archive.read(name)
                    assert png[:8]==b'\x89PNG\r\n\x1a\n'
                    assert png[25] in (0,2), 'App icons must be opaque grayscale/RGB PNGs'
                    sizes.add(struct.unpack('>II',png[16:24]))
            assert required<=sizes, f'Missing required {idiom} icon dimensions: {required-sizes}'
        with tempfile.TemporaryDirectory(prefix='papr-ipa-verify-') as temporary:
            binary=Path(temporary)/'Papr'
            binary.write_bytes(archive.read('Payload/Papr.app/Papr'))
            arch=subprocess.check_output(['file',str(binary)],text=True)
            assert 'arm64' in arch and 'Mach-O' in arch
            deployment=subprocess.check_output(['xcrun','vtool','-show-build',str(binary)],text=True)
            assert re.search(r'platform\s+IOS\b',deployment)
            assert re.search(r'minos\s+15\.0\b',deployment)
            signature=subprocess.run(['codesign','-d',str(binary)],stdout=subprocess.PIPE,stderr=subprocess.STDOUT,text=True)
            assert signature.returncode!=0 and 'not signed at all' in signature.stdout
            dependencies=subprocess.check_output(['otool','-L',str(binary)],text=True).splitlines()[1:]
            assert all(line.strip().startswith(('/System/Library/','/usr/lib/')) for line in dependencies)
    print('Verified: arm64 iPhoneOS executable, iOS 15.0 minimum (compatible with 18.3), system-only dependencies, unsigned IPA, no local database or credentials.')

def main():
    if sys.platform != 'darwin':
        raise SystemExit('Run this build on macOS with Xcode installed.')
    OUTPUT.mkdir(parents=True, exist_ok=True)
    # Apply the same minimum to Rust, bundled C dependencies and Swift plugins.
    os.environ['IPHONEOS_DEPLOYMENT_TARGET'] = MIN_IOS
    cargo = shutil.which('cargo') or str(Path.home()/'.cargo/bin/cargo')
    sdk = subprocess.check_output(['xcrun','--sdk','iphoneos','--show-sdk-path'],text=True).strip()
    # Xcode's Python launcher may inherit the macOS SDKROOT. Rust's dylib link
    # must use the device SDK too, even though the IPA uses only its staticlib.
    os.environ['SDKROOT'] = sdk
    developer = subprocess.check_output(['xcode-select','-p'],text=True).strip()
    # The Rust staticlib also includes the native Swift Tauri/plugin libraries.
    # Ask rustc for the exact native libraries needed by this dependency graph.
    if '--link-only' in sys.argv:
        output = (OUTPUT/'rust-build.log').read_text()
    else:
        run(['node','node_modules/typescript/bin/tsc'])
        run(['node','node_modules/vite/bin/vite.js','build'])
        try:
            result = run([cargo,'rustc','-p','papr','--lib','--target',IOS_TARGET,'--release',
                  '--features','tauri/custom-protocol','--','--print','native-static-libs'],
                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        except subprocess.CalledProcessError as error:
            (OUTPUT/'rust-build.log').write_text(error.stdout or '')
            print(error.stdout or '')
            raise
        output = result.stdout
        (OUTPUT/'rust-build.log').write_text(output)
    match = re.search(r'native-static-libs: (.+)',output)
    if not match:
        print(output)
        raise SystemExit('rustc did not report the native link libraries.')
    native = shlex.split(match.group(1))
    if APP.exists():
        shutil.rmtree(APP)
    APP.mkdir(parents=True)
    launcher = OUTPUT/'main.mm'
    launcher.write_text('extern "C" void start_app();\nint main(int argc, char **argv) { start_app(); return 0; }\n')
    toolchain = Path(developer)/'Toolchains/XcodeDefault.xctoolchain/usr/lib'
    clang_lib = subprocess.check_output(['xcrun','clang','--print-resource-dir'],text=True).strip()
    run(['xcrun','--sdk','iphoneos','clang++','-target',f'arm64-apple-ios{MIN_IOS}',
         '-isysroot',sdk,'-Os',launcher,
         ROOT/'target'/IOS_TARGET/'release/libpapr_lib.a',
         '-L'+str(toolchain/'swift/iphoneos'),'-L'+str(Path(sdk)/'usr/lib/swift'),
         '-L'+str(Path(clang_lib)/'lib/darwin'),
         '-Wl,-rpath,/usr/lib/swift','-Wl,-dead_strip','-Wl,-no_adhoc_codesign',
         *native,'-o',APP/'Papr'])
    run(['xcrun','strip','-x',APP/'Papr'])
    config=json.loads((ROOT/'src-tauri/tauri.conf.json').read_text())
    info={
        'CFBundleDevelopmentRegion':'en','CFBundleExecutable':'Papr',
        'CFBundleIdentifier':config['identifier'],'CFBundleInfoDictionaryVersion':'6.0',
        'CFBundleName':'Papr','CFBundleDisplayName':'Papr','CFBundlePackageType':'APPL',
        'CFBundleShortVersionString':config['version'],'CFBundleVersion':'8',
        'MinimumOSVersion':MIN_IOS,'LSRequiresIPhoneOS':True,'UIDeviceFamily':[1,2],
        'CFBundleSupportedPlatforms':['iPhoneOS'],'UIRequiredDeviceCapabilities':['arm64'],
        # iOS 14+ dictionary-based launch screens avoid a compiled storyboard.
        'UILaunchScreen':{},
        'UISupportedInterfaceOrientations':['UIInterfaceOrientationPortrait','UIInterfaceOrientationLandscapeLeft','UIInterfaceOrientationLandscapeRight'],
        'UISupportedInterfaceOrientations~ipad':['UIInterfaceOrientationPortrait','UIInterfaceOrientationPortraitUpsideDown','UIInterfaceOrientationLandscapeLeft','UIInterfaceOrientationLandscapeRight'],
        'CFBundleURLTypes':[{'CFBundleURLName':config['identifier'],'CFBundleURLSchemes':['papr']}],
    }
    info.update(plistlib.loads((ROOT/'src-tauri/Info.ios.plist').read_bytes()))
    icons=ROOT/'src-tauri/gen/apple/Assets.xcassets/AppIcon.appiconset'
    if not icons.exists():
        raise SystemExit('Run `tauri ios init` once to generate iOS icons.')
    icon_files={'iphone':[], 'ipad':[]}
    for entry in json.loads((icons/'Contents.json').read_text())['images']:
        if entry.get('filename') and entry.get('idiom') in icon_files:
            name=entry['filename'];shutil.copy2(icons/name,APP/name)
            stem=Path(name).stem
            if stem not in icon_files[entry['idiom']]:icon_files[entry['idiom']].append(stem)
    # Keep iOS app icons opaque, and keep iPhone/iPad declarations separate.
    mac_sdk=subprocess.check_output(['xcrun','--sdk','macosx','--show-sdk-path'],text=True).strip()
    run(['xcrun','--sdk','macosx','swift',ROOT/'scripts/opaque-ios-icons.swift',APP],env={**os.environ,'SDKROOT':mac_sdk})
    info['CFBundleIcons']={'CFBundlePrimaryIcon':{'CFBundleIconFiles':icon_files['iphone'],'UIPrerenderedIcon':False}}
    info['CFBundleIcons~ipad']={'CFBundlePrimaryIcon':{'CFBundleIconFiles':icon_files['ipad'],'UIPrerenderedIcon':False}}
    info['CFBundleIconFiles']=icon_files['iphone']
    (APP/'Info.plist').write_bytes(plistlib.dumps(info))
    (APP/'PkgInfo').write_bytes(b'APPL????')
    # Ensure even an automatically added ad-hoc signature is removed.
    probe=subprocess.run(['codesign','-d',str(APP/'Papr')],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    if probe.returncode==0:
        run(['codesign','--remove-signature',APP/'Papr'])
    ipa=OUTPUT/f'Papr_{config["version"]}_iOS_unsigned.ipa'
    if ipa.exists():ipa.unlink()
    run(['ditto','-c','-k','--keepParent',OUTPUT/'Payload',ipa])
    verify(ipa)
    print(f'IPA: {ipa}\nSHA256: {hashlib.sha256(ipa.read_bytes()).hexdigest()}')

if __name__=='__main__':
    main()
