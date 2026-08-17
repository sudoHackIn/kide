package dev.kide.fixture.book;

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
final class JavaBookAuditController {
    private final JavaBookAudit audit = new SpringJavaBookAudit();

    String create(String title) {
        return audit.record(title);
    }
}
