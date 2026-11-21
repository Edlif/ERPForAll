// SPDX-License-Identifier: MIT OR Apache-2.0
import com.adobe.internal.xmp.XMPMeta;
import com.adobe.internal.xmp.XMPMetaFactory;
import com.adobe.internal.xmp.options.SerializeOptions;
import java.io.ByteArrayOutputStream;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import org.apache.pdfbox.Loader;
import org.apache.pdfbox.pdmodel.PDDocument;
import org.apache.pdfbox.pdmodel.PDPage;
import org.apache.pdfbox.pdmodel.common.PDMetadata;
import org.apache.xmpbox.XMPMetadata;
import org.apache.xmpbox.xml.XmpSerializer;

class Generate {
    static final String PDF = "http://ns.adobe.com/pdf/1.3/";
    static final String SOURCE = "https://example.org/erpforall/fixture/1.0/";

    // The respective production libraries create all XML and all PDF syntax.
    static void writeDocument(String name, byte[] packet) throws Exception {
        var file = Path.of(name + ".pdf").toFile();
        try (var document = new PDDocument()) {
            document.addPage(new PDPage());
            var metadata = new PDMetadata(document);
            metadata.importXMPMetadata(packet);
            document.getDocumentCatalog().setMetadata(metadata);
            document.save(file);
        }
        try (var saved = Loader.loadPDF(file);
             var stream = saved.getDocumentCatalog().getMetadata().exportXMPMetadata()) {
            byte[] extracted = stream.readAllBytes();
            if (!Arrays.equals(extracted, packet)) throw new IllegalStateException(name);
            Files.write(Path.of(name + ".xmp"), extracted);
        }
    }

    public static void main(String[] args) throws Exception {
        var empty = new ByteArrayOutputStream();
        new XmpSerializer().serialize(XMPMetadata.createXMPMetadata(), empty, true);
        writeDocument("empty", empty.toByteArray());

        XMPMetaFactory.getSchemaRegistry().registerNamespace(SOURCE, "sample");
        XMPMeta profile = XMPMetaFactory.create();
        profile.setProperty(PDF, "Producer", "Document tool");
        profile.setQualifier(PDF, "Producer", SOURCE, "edition", "Library");
        profile.setStructField(SOURCE, "sourceDocument", PDF, "Producer", "Source tool");
        writeDocument("qualified-and-nested",
            XMPMetaFactory.serializeToBuffer(profile, new SerializeOptions().setPadding(0)));
    }
}
