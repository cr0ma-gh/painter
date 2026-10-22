// Finder thumbnails of Painter projects: the PNG thumbnail Painter stores,
// uncompressed, as the first entry of every .painter file (a ZIP).
import AppKit
import QuickLookThumbnailing

class ThumbnailProvider: QLThumbnailProvider {
    override func provideThumbnail(for request: QLFileThumbnailRequest, _ handler: @escaping (QLThumbnailReply?, Error?) -> Void) {
        guard let data = try? Data(contentsOf: request.fileURL, options: .mappedIfSafe),
              let png = ThumbnailProvider.firstStoredEntry(named: "thumbnail.png", in: data),
              let image = NSImage(data: png),
              image.size.width > 0, image.size.height > 0
        else {
            handler(nil, nil)
            return
        }
        let size = image.size
        let scale = min(request.maximumSize.width / size.width, request.maximumSize.height / size.height, 1)
        let target = CGSize(width: floor(size.width * scale), height: floor(size.height * scale))
        handler(QLThumbnailReply(contextSize: target, currentContextDrawing: { () -> Bool in
            image.draw(in: CGRect(origin: .zero, size: target))
            return true
        }), nil)
    }

    /// The ZIP's first entry when it is `name` and stored uncompressed.
    static func firstStoredEntry(named name: String, in data: Data) -> Data? {
        func u16(_ at: Int) -> Int { Int(data[data.startIndex + at]) | Int(data[data.startIndex + at + 1]) << 8 }
        func u32(_ at: Int) -> Int { u16(at) | u16(at + 2) << 16 }
        guard data.count > 30, u32(0) == 0x0403_4b50, u16(8) == 0 else { return nil }
        let size = u32(18), nameLength = u16(26), extraLength = u16(28)
        let start = 30 + nameLength + extraLength
        guard start + size <= data.count,
              String(data: data.subdata(in: data.startIndex + 30 ..< data.startIndex + 30 + nameLength), encoding: .utf8) == name
        else { return nil }
        return data.subdata(in: data.startIndex + start ..< data.startIndex + start + size)
    }
}
