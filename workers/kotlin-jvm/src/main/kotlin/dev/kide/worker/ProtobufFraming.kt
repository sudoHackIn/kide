package dev.kide.worker

import java.io.EOFException
import java.io.InputStream
import java.io.OutputStream
import kide.worker.v1.Worker

/** Unsigned-varint-length-delimited frames used exclusively on the worker pipe. */
internal object ProtobufFraming {
    fun read(input: InputStream): Worker.Envelope? {
        val first = input.read()
        if (first == -1) return null
        var length = (first and 0x7f).toLong()
        var shift = 7
        var current = first
        while (current and 0x80 != 0) {
            require(shift < 64) { "invalid protobuf frame length" }
            current = input.read().also { if (it == -1) throw EOFException("truncated protobuf frame length") }
            length = length or ((current and 0x7f).toLong() shl shift)
            shift += 7
        }
        require(length <= Int.MAX_VALUE) { "protobuf frame exceeds JVM array limit" }
        val payload = input.readNBytes(length.toInt())
        if (payload.size != length.toInt()) throw EOFException("truncated protobuf frame payload")
        return Worker.Envelope.parseFrom(payload)
    }

    fun write(output: OutputStream, envelope: Worker.Envelope) {
        val payload = envelope.toByteArray()
        var length = payload.size
        while (length >= 0x80) {
            output.write((length and 0x7f) or 0x80)
            length = length ushr 7
        }
        output.write(length)
        output.write(payload)
        output.flush()
    }
}
