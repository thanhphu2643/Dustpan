// Draws Resources/AppIcon.icns: a straw broom sweeping dust on a cream tile.
// Run: swift scripts/make-icon.swift
import AppKit

func color(_ r: CGFloat, _ g: CGFloat, _ b: CGFloat, _ a: CGFloat = 1) -> NSColor {
    NSColor(red: r, green: g, blue: b, alpha: a)
}

func render(_ size: Int) -> Data {
    let s = CGFloat(size) / 1024
    let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: size, pixelsHigh: size, bitsPerSample: 8,
                               samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                               bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    let ctx = NSGraphicsContext.current!.cgContext
    ctx.scaleBy(x: s, y: s)

    // Tile on Apple's icon grid: 824pt body, 100pt margin, soft drop shadow.
    let tile = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = NSBezierPath(roundedRect: tile, xRadius: 185, yRadius: 185)
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: NSColor.black.withAlphaComponent(0.3).cgColor)
    NSColor.black.setFill()
    shape.fill()
    ctx.restoreGState()
    ctx.saveGState()
    shape.addClip()
    NSGradient(colors: [color(1.0, 0.97, 0.91), color(0.95, 0.88, 0.77)])!.draw(in: tile, angle: -90)

    // Dust kicked up in front of the bristles.
    color(0.62, 0.55, 0.48, 0.55).setFill()
    for (x, y, r) in [(232.0, 282.0, 30.0), (300.0, 228.0, 22.0), (196.0, 352.0, 17.0), (262.0, 196.0, 13.0), (178.0, 248.0, 11.0)] {
        NSBezierPath(ovalIn: CGRect(x: x - r, y: y - r, width: 2 * r, height: 2 * r)).fill()
    }

    // The broom, drawn upright around the band and then tilted.
    ctx.translateBy(x: 560, y: 520)
    ctx.rotate(by: -30 * .pi / 180)
    ctx.setShadow(offset: CGSize(width: 10, height: -14), blur: 24, color: color(0.35, 0.22, 0.10, 0.35).cgColor)
    ctx.beginTransparencyLayer(auxiliaryInfo: nil) // one shadow for the whole broom

    // Handle.
    let handle = NSBezierPath(roundedRect: CGRect(x: -24, y: 30, width: 48, height: 330), xRadius: 24, yRadius: 24)
    NSGradient(colors: [color(0.86, 0.60, 0.33), color(0.64, 0.39, 0.18)])!.draw(in: handle, angle: 0)

    // Straw head: narrow at the band, flaring out to a slightly rounded edge.
    let head = NSBezierPath()
    head.move(to: CGPoint(x: -72, y: 20))
    head.line(to: CGPoint(x: 72, y: 20))
    head.curve(to: CGPoint(x: 160, y: -240), controlPoint1: CGPoint(x: 110, y: -60), controlPoint2: CGPoint(x: 150, y: -150))
    head.curve(to: CGPoint(x: -160, y: -240), controlPoint1: CGPoint(x: 60, y: -268), controlPoint2: CGPoint(x: -60, y: -268))
    head.curve(to: CGPoint(x: -72, y: 20), controlPoint1: CGPoint(x: -150, y: -150), controlPoint2: CGPoint(x: -110, y: -60))
    head.close()
    NSGradient(colors: [color(0.99, 0.80, 0.36), color(0.93, 0.58, 0.16)])!.draw(in: head, angle: -90)

    // Individual straws.
    ctx.saveGState()
    head.addClip()
    color(0.74, 0.44, 0.10, 0.55).setStroke()
    for i in -5...5 {
        let t = CGFloat(i) / 5
        let straw = NSBezierPath()
        straw.move(to: CGPoint(x: t * 60, y: 10))
        straw.line(to: CGPoint(x: t * 150, y: -270))
        straw.lineWidth = 7
        straw.lineCapStyle = .round
        straw.stroke()
    }
    // Stitching across the straw.
    let stitch = NSBezierPath()
    stitch.move(to: CGPoint(x: -140, y: -70))
    stitch.curve(to: CGPoint(x: 140, y: -70), controlPoint1: CGPoint(x: -50, y: -92), controlPoint2: CGPoint(x: 50, y: -92))
    stitch.lineWidth = 16
    color(0.76, 0.24, 0.16).setStroke()
    stitch.stroke()
    ctx.restoreGState()

    // Band holding head and handle together.
    let band = NSBezierPath(roundedRect: CGRect(x: -80, y: 4, width: 160, height: 56), xRadius: 18, yRadius: 18)
    NSGradient(colors: [color(0.86, 0.32, 0.20), color(0.68, 0.20, 0.13)])!.draw(in: band, angle: -90)

    ctx.endTransparencyLayer()
    ctx.restoreGState()

    NSGraphicsContext.restoreGraphicsState()
    return rep.representation(using: .png, properties: [:])!
}

let root = URL(fileURLWithPath: CommandLine.arguments[0]).deletingLastPathComponent().deletingLastPathComponent()
let iconset = FileManager.default.temporaryDirectory.appendingPathComponent("DustpanIcon.iconset")
try? FileManager.default.removeItem(at: iconset)
try! FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)
for base in [16, 32, 128, 256, 512] {
    try! render(base).write(to: iconset.appendingPathComponent("icon_\(base)x\(base).png"))
    try! render(base * 2).write(to: iconset.appendingPathComponent("icon_\(base)x\(base)@2x.png"))
}
let out = root.appendingPathComponent("Resources/AppIcon.icns")
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", out.path]
try! task.run()
task.waitUntilExit()
try! render(1024).write(to: root.appendingPathComponent("Resources/AppIcon.png"))
print("Wrote \(out.path)")
