#!/usr/bin/env swift
// AppIcon 生成スクリプト
// "Vantage Point" — 山を登る最適ルートを探すイメージ
// ダークな山のシルエット + 光るルートライン（開発の Waves）
//
// 使い方: swift scripts/generate-icon.swift

import AppKit

let sizes = [128, 256, 512, 1024]
let outputDir = "Resources/Assets.xcassets/AppIcon.appiconset"

for size in sizes {
    let s = CGFloat(size)

    let image = NSImage(size: NSSize(width: s, height: s))
    image.lockFocus()

    guard let ctx = NSGraphicsContext.current?.cgContext else {
        print("❌ Failed to get graphics context")
        continue
    }

    let colorSpace = CGColorSpaceCreateDeviceRGB()

    // === 背景: 深い夜空グラデーション ===
    let bgColors = [
        CGColor(red: 0.05, green: 0.05, blue: 0.10, alpha: 1.0),  // 上: 深い黒
        CGColor(red: 0.10, green: 0.08, blue: 0.20, alpha: 1.0),  // 中: 暗い紫
        CGColor(red: 0.14, green: 0.10, blue: 0.26, alpha: 1.0),  // 下: 紫
    ] as CFArray
    if let gradient = CGGradient(colorsSpace: colorSpace, colors: bgColors, locations: [0, 0.6, 1]) {
        ctx.drawLinearGradient(gradient,
            start: CGPoint(x: s/2, y: s),
            end: CGPoint(x: s/2, y: 0),
            options: [])
    }

    // === 山のシルエット（後方・大きい山） ===
    let mountain1 = CGMutablePath()
    mountain1.move(to: CGPoint(x: s * 0.25, y: s * 0.22))
    mountain1.addLine(to: CGPoint(x: s * 0.58, y: s * 0.78))
    mountain1.addLine(to: CGPoint(x: s * 0.52, y: s * 0.72))  // 山頂の凹み
    mountain1.addLine(to: CGPoint(x: s * 0.48, y: s * 0.75))
    mountain1.addLine(to: CGPoint(x: s * 0.92, y: s * 0.22))
    mountain1.addLine(to: CGPoint(x: s * 0.25, y: s * 0.22))
    mountain1.closeSubpath()

    ctx.setFillColor(CGColor(red: 0.18, green: 0.16, blue: 0.28, alpha: 1.0))
    ctx.addPath(mountain1)
    ctx.fillPath()

    // === 山のシルエット（前方・小さい山） ===
    let mountain2 = CGMutablePath()
    mountain2.move(to: CGPoint(x: s * 0.05, y: s * 0.22))
    mountain2.addLine(to: CGPoint(x: s * 0.30, y: s * 0.58))
    mountain2.addLine(to: CGPoint(x: s * 0.26, y: s * 0.54))  // 山頂の凹み
    mountain2.addLine(to: CGPoint(x: s * 0.23, y: s * 0.56))
    mountain2.addLine(to: CGPoint(x: s * 0.55, y: s * 0.22))
    mountain2.addLine(to: CGPoint(x: s * 0.05, y: s * 0.22))
    mountain2.closeSubpath()

    ctx.setFillColor(CGColor(red: 0.22, green: 0.20, blue: 0.34, alpha: 1.0))
    ctx.addPath(mountain2)
    ctx.fillPath()

    // === ルートライン（光る最適パス — 開発の Waves） ===
    let route = CGMutablePath()
    // 山麓から山頂へ、波打ちながら登るルート
    route.move(to: CGPoint(x: s * 0.72, y: s * 0.24))
    route.addCurve(
        to: CGPoint(x: s * 0.66, y: s * 0.44),
        control1: CGPoint(x: s * 0.68, y: s * 0.34),
        control2: CGPoint(x: s * 0.74, y: s * 0.38))
    route.addCurve(
        to: CGPoint(x: s * 0.60, y: s * 0.62),
        control1: CGPoint(x: s * 0.58, y: s * 0.50),
        control2: CGPoint(x: s * 0.65, y: s * 0.56))
    route.addCurve(
        to: CGPoint(x: s * 0.55, y: s * 0.76),
        control1: CGPoint(x: s * 0.55, y: s * 0.68),
        control2: CGPoint(x: s * 0.58, y: s * 0.72))

    // ルートの光彩（グロー）
    ctx.saveGState()
    ctx.setLineWidth(s * 0.025)
    ctx.setStrokeColor(CGColor(red: 0.3, green: 0.6, blue: 1.0, alpha: 0.3))
    ctx.setShadow(offset: .zero, blur: s * 0.04,
        color: CGColor(red: 0.3, green: 0.6, blue: 1.0, alpha: 0.6))
    ctx.addPath(route)
    ctx.strokePath()
    ctx.restoreGState()

    // ルートの本体ライン
    ctx.setLineWidth(s * 0.012)
    ctx.setLineCap(.round)
    ctx.setLineJoin(.round)
    ctx.setStrokeColor(CGColor(red: 0.4, green: 0.75, blue: 1.0, alpha: 0.9))
    ctx.addPath(route)
    ctx.strokePath()

    // === ルート上のドット（ウェイポイント） ===
    let waypoints = [
        CGPoint(x: s * 0.72, y: s * 0.24),
        CGPoint(x: s * 0.66, y: s * 0.44),
        CGPoint(x: s * 0.60, y: s * 0.62),
        CGPoint(x: s * 0.55, y: s * 0.76),  // 山頂
    ]
    for (i, pt) in waypoints.enumerated() {
        let dotSize = s * (i == waypoints.count - 1 ? 0.025 : 0.015)
        // グロー
        ctx.saveGState()
        ctx.setShadow(offset: .zero, blur: s * 0.02,
            color: CGColor(red: 0.4, green: 0.8, blue: 1.0, alpha: 0.8))
        ctx.setFillColor(CGColor(red: 0.5, green: 0.85, blue: 1.0, alpha: 1.0))
        ctx.fillEllipse(in: CGRect(
            x: pt.x - dotSize, y: pt.y - dotSize,
            width: dotSize * 2, height: dotSize * 2))
        ctx.restoreGState()
    }

    // === VP テキスト（小さく下部に） ===
    let vpFont = CTFontCreateWithName("SF Pro Display" as CFString, s * 0.08, nil)
    let vpAttrs: [NSAttributedString.Key: Any] = [
        .font: vpFont,
        .foregroundColor: NSColor(red: 0.6, green: 0.65, blue: 0.75, alpha: 0.6),
    ]
    let vpStr = NSAttributedString(string: "VP", attributes: vpAttrs)
    let vpSize = vpStr.size()
    vpStr.draw(at: NSPoint(x: (s - vpSize.width) / 2, y: s * 0.08))

    image.unlockFocus()

    // PNG 書き出し
    guard let tiffData = image.tiffRepresentation,
          let bitmap = NSBitmapImageRep(data: tiffData),
          let pngData = bitmap.representation(using: .png, properties: [:]) else {
        print("❌ Failed to generate PNG for size \(size)")
        continue
    }

    let filename = "\(outputDir)/AppIcon-\(size).png"
    do {
        try pngData.write(to: URL(fileURLWithPath: filename))
        print("✅ Generated: \(filename)")
    } catch {
        print("❌ Failed to write \(filename): \(error)")
    }
}

print("🎨 App icon generation complete!")
