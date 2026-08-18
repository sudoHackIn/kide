package dev.kide.fixture

import org.springframework.beans.factory.annotation.Qualifier
import org.springframework.boot.autoconfigure.AutoConfiguration
import org.springframework.context.annotation.Bean
import org.springframework.context.annotation.Configuration
import org.springframework.context.annotation.Primary
import org.springframework.stereotype.Component
import org.springframework.stereotype.Repository
import org.springframework.stereotype.Service

@Component
class IndexedComponent

@Service
class IndexedService

@Repository
class IndexedRepository

@Qualifier("indexed")
class QualifiedComponent

@AutoConfiguration
class IndexedAutoConfiguration

@Configuration
class IndexedConfiguration {
    @Bean
    @Primary
    fun indexedBean(): IndexedComponent = IndexedComponent()
}
