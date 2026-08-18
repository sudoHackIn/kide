package dev.kide.fixture.domain;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import org.springframework.data.jpa.repository.JpaRepository;

@Entity
public record Book(@Id Long id, String title) {}

interface BookRepository extends JpaRepository<Book, Long> {}
