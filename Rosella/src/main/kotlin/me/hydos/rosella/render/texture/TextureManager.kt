package me.hydos.rosella.render.texture

import it.unimi.dsi.fastutil.ints.IntArrayPriorityQueue
import it.unimi.dsi.fastutil.ints.IntPriorityQueues
import me.hydos.rosella.memory.Memory

import me.hydos.rosella.render.*
import me.hydos.rosella.render.renderer.Renderer
import org.lwjgl.vulkan.VK10
import me.hydos.rosella.vkobjects.VkCommon

/**
 * Caches Textures and other texture related objects
 */
class TextureManager(val common: VkCommon) { // TODO: add layers, maybe not in this class but somewhere

	private val textureMap = HashMap<Int, Texture>()
	private val samplerCache = HashMap<SamplerCreateInfo, TextureSampler>() // bro there's like 3 options for this

	private val preparedTextures = HashSet<Texture>()

	private val reusableTexIds = IntPriorityQueues.synchronize(IntArrayPriorityQueue())
	private var nextTexId : Int = 0;

	fun generateTextureId(): Int {
		return if (!reusableTexIds.isEmpty) {
			reusableTexIds.dequeueInt()
		} else {
			nextTexId++
		}
	}

	fun deleteTexture(textureId: Int) {
		val removedTex = textureMap.remove(textureId)
		preparedTextures.remove(removedTex)
		reusableTexIds.enqueue(textureId)
	}

	fun getTexture(textureId: Int): Texture? {
		return textureMap[textureId];
	}

	fun createTexture(
		renderer: Renderer,
		textureId: Int,
		width: Int,
		height: Int,
		imgFormat: Int,
		samplerCreateInfo: SamplerCreateInfo
	) {
		val textureImage = TextureImage(0, 0, 0)

		createTextureImage(renderer, common.device, width, height, imgFormat, textureImage)
		textureImage.view = createTextureImageView(common.device, imgFormat, textureImage.textureImage)

		val textureSampler = samplerCache.computeIfAbsent(samplerCreateInfo) {
			TextureSampler(samplerCreateInfo, common.device)
		}

		textureMap[textureId] = Texture(imgFormat, width, height, textureImage, textureSampler.pointer);
	}

	fun applySamplerInfoToTexture(
		textureId: Int,
		samplerCreateInfo: SamplerCreateInfo
	) {
		val textureSampler = samplerCache.computeIfAbsent(samplerCreateInfo) {
			TextureSampler(samplerCreateInfo, common.device)
		}

		textureMap[textureId]?.textureSampler = textureSampler.pointer
	}

	fun drawToExistingTexture(
		renderer: Renderer,
		memory: Memory,
		textureId: Int,
		image: UploadableImage,
		srcRegion: ImageRegion,
		dstRegion: ImageRegion,
	) {
		val texture = getTexture(textureId)!!
		if (preparedTextures.contains(texture)) {
			transitionImageLayout(
				texture.textureImage.textureImage,
				texture.imgFormat,
				VK10.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
				VK10.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
				renderer.depthBuffer,
				common.device,
				renderer
			)
			preparedTextures.remove(texture)
		}
		drawToTexture(
			renderer,
			common.device,
			memory,
			image,
			srcRegion,
			dstRegion,
			texture)
	}

	fun drawToExistingTexture(
		renderer: Renderer,
		memory: Memory,
		textureId: Int,
		image: UploadableImage
	) {
		val region = ImageRegion(0, 0, image.getWidth(), image.getHeight())
		drawToExistingTexture(renderer, memory, textureId, image, region, region)
	}

	fun prepareTexture(
		renderer: Renderer,
		texture: Texture
	) {
		if (!preparedTextures.contains(texture)) {
			transitionImageLayout(
				texture.textureImage.textureImage,
				texture.imgFormat,
				VK10.VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
				VK10.VK_IMAGE_LAYOUT_SHADER_READ_ONLY_OPTIMAL,
				renderer.depthBuffer,
				common.device,
				renderer
			)
			preparedTextures.add(texture)
		}
	}
}