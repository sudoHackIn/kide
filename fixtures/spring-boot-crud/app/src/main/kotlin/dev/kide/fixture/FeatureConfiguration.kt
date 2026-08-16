package dev.kide.fixture

import org.springframework.boot.autoconfigure.condition.ConditionalOnProperty
import org.springframework.context.annotation.Configuration

@ConditionalOnProperty(name = ["feature.books"])
class FeatureConfiguration {
    @Configuration
    class NestedBookConfiguration
}
