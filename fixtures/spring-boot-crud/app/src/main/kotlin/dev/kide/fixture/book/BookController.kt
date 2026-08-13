package dev.kide.fixture.book

import jakarta.validation.Valid
import jakarta.validation.constraints.NotBlank
import org.springframework.data.jpa.repository.JpaRepository
import org.springframework.http.HttpStatus
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PathVariable
import org.springframework.web.bind.annotation.PostMapping
import org.springframework.web.bind.annotation.RequestBody
import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.ResponseStatus
import org.springframework.web.bind.annotation.RestController

interface BookRepository : JpaRepository<BookEntity, Long>

data class CreateBookRequest(@field:NotBlank val title: String, @field:NotBlank val author: String)

@RestController
@RequestMapping("/books")
class BookController(private val repository: BookRepository) {
    @GetMapping
    fun list(): List<BookEntity> = repository.findAll()

    @GetMapping("/{id}")
    fun get(@PathVariable id: Long): BookEntity = repository.findById(id).orElseThrow()

    @PostMapping
    @ResponseStatus(HttpStatus.CREATED)
    fun create(@Valid @RequestBody request: CreateBookRequest): BookEntity =
        repository.save(BookEntity(title = request.title, author = request.author))
}
