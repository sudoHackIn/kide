# Spring Boot CRUD fixture

Небольшой Kotlin/Gradle multi-module fixture для integration-проверок KIDE.

- `domain` — отдельный Kotlin-модуль и Jackson annotations;
- `app` — Spring Boot CRUD с Web MVC, Validation, JPA, Actuator, H2 и PostgreSQL runtime driver;
- тест запускает Spring context на in-memory H2, без внешних сервисов.

Проверка из корня KIDE:

```bash
./workers/kotlin-jvm/gradlew --project-dir fixtures/spring-boot-crud test
```
