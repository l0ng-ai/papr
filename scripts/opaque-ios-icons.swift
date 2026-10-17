// Preserve the vector-rendered Papr artwork while encoding opaque RGB PNGs.
import Foundation
import CoreGraphics
import ImageIO

let folder = URL(fileURLWithPath: CommandLine.arguments[1])
for url in try FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil) where url.pathExtension.lowercased() == "png" {
    guard let source = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(source, 0, nil),
          let context = CGContext(data: nil, width: image.width, height: image.height,
            bitsPerComponent: 8, bytesPerRow: image.width * 4,
            space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else {
        fatalError("Cannot decode icon: \(url.lastPathComponent)")
    }
    let rect = CGRect(x: 0, y: 0, width: image.width, height: image.height)
    context.setFillColor(CGColor(red: 244/255, green: 238/255, blue: 225/255, alpha: 1))
    context.fill(rect)
    context.draw(image, in: rect)
    guard let opaque = context.makeImage(),
          let destination = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil) else {
        fatalError("Cannot encode icon: \(url.lastPathComponent)")
    }
    CGImageDestinationAddImage(destination, opaque, nil)
    guard CGImageDestinationFinalize(destination) else { fatalError("Icon encoding failed") }
}
