package dev.kide.fixture.book;

import org.springframework.web.bind.annotation.RestController;

@interface Audited {
}

public interface JavaBookAudit {
    String record(String title);
}

final class SpringJavaBookAudit implements JavaBookAudit {
    @Override
    public String record(String title) {
        return title.trim();
    }
}

@Audited
@RestController
final class JavaBookAuditController {
    private final JavaBookAudit audit = new SpringJavaBookAudit();

    String create(String title) {
        return audit.record(title);
    }
}
