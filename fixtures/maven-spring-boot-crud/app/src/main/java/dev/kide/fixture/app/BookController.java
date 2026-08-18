package dev.kide.fixture.app;

import dev.kide.fixture.domain.Book;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.RestController;

@RestController
public class BookController {
    @GetMapping("/books/example")
    public Book example() { return new Book(1L, "Maven CRUD"); }
}
