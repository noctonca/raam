// Writes the web demo's faces.json: every face Apple's Vision framework
// finds in the bundled photos, as the web host's TileSource reads it (the
// stand-in for Immich's /api/faces). macOS only; run it again whenever
// the photos change:
//
//     swift tools/web-faces.swift hosts/web/www/photos > hosts/web/www/faces.json
//
// Each box is [x1, y1, x2, y2] in 0..1 fractions of the photo with y = 0
// at the top (Vision's own origin is the bottom-left). The web source
// turns them into a focus with raam-model's Focus::from_faces, Frameo's
// rule, exactly as the engine does with Immich's boxes.
import Foundation
import ImageIO
import Vision

let dir = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "."
let files = try FileManager.default.contentsOfDirectory(atPath: dir)
    .filter { $0.hasSuffix(".jpg") }
    .sorted()

func r4(_ v: CGFloat) -> Double { (Double(v) * 10000).rounded() / 10000 }

var out: [[String: Any]] = []
for name in files {
    let url = URL(fileURLWithPath: dir).appendingPathComponent(name)
    guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
          let image = CGImageSourceCreateImageAtIndex(src, 0, nil)
    else {
        FileHandle.standardError.write("\(name): not an image\n".data(using: .utf8)!)
        exit(1)
    }
    let request = VNDetectFaceRectanglesRequest()
    try VNImageRequestHandler(cgImage: image, options: [:]).perform([request])
    let faces = (request.results ?? [])
        .map { $0.boundingBox }
        .sorted { $0.minX < $1.minX }
        .map { b in [r4(b.minX), r4(1 - b.maxY), r4(b.maxX), r4(1 - b.minY)] }
    out.append([
        "file": name,
        "width": image.width,
        "height": image.height,
        "faces": faces,
    ])
}
let json = try JSONSerialization.data(withJSONObject: out, options: [.prettyPrinted, .sortedKeys])
print(String(data: json, encoding: .utf8)!)
